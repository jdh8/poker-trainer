//! The training loops.
//!
//! - `run_pot_odds_drill`: deal a hand + flop from a solved heads-up preflop
//!   equilibrium, villain bets, you call or fold; scored against break-even pot
//!   odds using your Monte-Carlo equity vs villain's *whole range* at the node.
//! - `run_gto_drill`: act vs. a precomputed solution; scored on EV loss (Phase 1).

use crate::eval::{self, Bucket};
use crate::postflop_table::PostflopTable;
use crate::preflop::{self, PreflopCharts, PreflopNode};
use crate::solution::{
    FileSolutionProvider, LiveSolutionProvider, NodeStrategy, SolutionProvider, SolveRequest,
    SolvedSpot,
};
use crate::stats;
use crate::texture;
use crate::tree::{TableWalk, TreeNode, TreeSession, TreeWalk};
use rand::seq::IndexedRandom;
use rand::RngExt;
use rs_poker::core::{Card, Deck, Suit};
use std::collections::BTreeMap;
#[cfg(feature = "tui")]
use std::io::IsTerminal;
use std::io::{self, Write};
#[cfg(feature = "tui")]
use std::path::PathBuf;

const BET_FRACTIONS: [f64; 5] = [0.33, 0.5, 0.75, 1.0, 1.5];

/// The heads-up ruleset the pot-odds drill draws from when `--preflop` is omitted.
const DEFAULT_POT_ODDS_RULESET: &str = "cash-hu89";

// Villain-range equity knobs (pot-odds scoring and range-drill sub-bucketing).
const EQ_ITERS: u32 = 40; // Monte-Carlo runouts per (hero, villain) pair
const EQ_VILLAIN_CAP: usize = 120; // sample at most this many villain combos
const SPLIT_MIN: usize = 6; // don't split a bucket smaller than this

/// Break-even equity to call: you risk `bet` to win `pot + bet`.
fn required_equity(pot: f64, bet: f64) -> f64 {
    bet / (pot + 2.0 * bet)
}

/// EV of calling, in bb (positive => calling is +EV).
fn call_ev(eq: f64, pot: f64, bet: f64) -> f64 {
    eq * (pot + bet) - (1.0 - eq) * bet
}

/// One step of a heads-up preflop forward simulation ([`sample_preflop_flop_spot`]):
/// given the acting node's geometry and the sampled action, does the betting
/// continue, die (fold/all-in — no bettable flop), or close into a flop of the
/// returned pot (bb)?
enum Step {
    Flop(f64),
    Continue,
    Dead,
}

/// Heads-up preflop betting closure. HU acts SB→BB→…; a `Call` closes the round
/// **except** the SB's opening complete at the root (`""`), where BB keeps the
/// option. `All-in` dies so the sim aborts before any jam is called — an all-in
/// pot (no postflop bet decision) never leaks through.
fn hu_step(node_path: &str, pot_bb: f32, to_call_bb: f32, action: &str) -> Step {
    match action {
        "Fold" | "All-in" => Step::Dead,
        "Check" => Step::Flop(pot_bb as f64),
        "Call" if node_path.is_empty() => Step::Continue, // SB open-limp
        "Call" => Step::Flop((pot_bb + to_call_bb) as f64),
        _ => Step::Continue, // a raise
    }
}

/// A flop spot drawn from a solved heads-up preflop line: both hands, the flop,
/// the pot the preflop betting closed into, the terminal action path, and each
/// seat's per-class reach along the line (the villain seat's is the range the
/// drill scores hero's equity against).
struct PreflopFlopSpot {
    sb: [Card; 2],
    bb: [Card; 2],
    flop: [Card; 3],
    pot: f64,
    path: String,
    sb_reach: Vec<f32>,
    bb_reach: Vec<f32>,
}

/// Forward-simulate one heads-up preflop hand from the chart root, sampling each
/// seat's action from the equilibrium (`freqs[action][class]`). Returns the flop
/// spot the betting closes into, or `None` if the hand ended preflop
/// (fold/all-in) or hit a pruned node — the caller just retries.
fn sample_preflop_flop_spot<R: RngExt>(
    charts: &PreflopCharts,
    stack_bb: Option<f64>,
    rng: &mut R,
) -> Option<PreflopFlopSpot> {
    let mut deck = Deck::default();
    let sb = [deck.deal(rng).unwrap(), deck.deal(rng).unwrap()];
    let bb = [deck.deal(rng).unwrap(), deck.deal(rng).unwrap()];
    let sb_class = preflop::class_index(sb);
    let bb_class = preflop::class_index(bb);

    // Each seat's per-class arrival probability: the product of its own action
    // frequencies along the sampled line (mirrors `PreflopCharts::class_reach`,
    // accumulated inline as we walk).
    let mut sb_reach = vec![1.0f32; preflop::CLASSES];
    let mut bb_reach = vec![1.0f32; preflop::CLASSES];

    let mut path = String::new();
    loop {
        let node = charts.node(&path)?; // pruned/missing => abort this attempt
        let class = if node.seat == "SB" {
            sb_class
        } else {
            bb_class
        };
        // Sample an action ∝ this class's frequencies at the node.
        let weights: Vec<f32> = node.freqs.iter().map(|f| f[class]).collect();
        let wsum: f32 = weights.iter().sum();
        if wsum <= 0.0 {
            return None; // this class never arrives here
        }
        let mut roll = rng.random_range(0.0..wsum);
        let ai = weights
            .iter()
            .position(|w| {
                roll -= w;
                roll < 0.0
            })
            .unwrap_or(weights.len() - 1);
        let action = &node.actions[ai];
        let tok = preflop::label_token(action);

        // Fold this action into the acting seat's reach over all 169 classes.
        let seat_reach = if node.seat == "SB" {
            &mut sb_reach
        } else {
            &mut bb_reach
        };
        for (r, f) in seat_reach.iter_mut().zip(&node.freqs[ai]) {
            *r *= f;
        }

        match hu_step(&path, node.pot_bb, node.to_call_bb, action) {
            Step::Dead => return None,
            Step::Flop(pot) => {
                // ponytail: coarse all-in guard; deep HU rulesets (cash-hu89)
                // don't hit it. Precise commitment tracking if we add
                // short-stack HU (a "Raise to X" can be all-in yet unlabeled).
                if stack_bb.is_some_and(|s| pot >= 2.0 * s) {
                    return None;
                }
                let flop = [
                    deck.deal(rng).unwrap(),
                    deck.deal(rng).unwrap(),
                    deck.deal(rng).unwrap(),
                ];
                let terminal = if path.is_empty() {
                    tok
                } else {
                    format!("{path}-{tok}")
                };
                return Some(PreflopFlopSpot {
                    sb,
                    bb,
                    flop,
                    pot,
                    path: terminal,
                    sb_reach,
                    bb_reach,
                });
            }
            Step::Continue => {
                if !path.is_empty() {
                    path.push('-');
                }
                path.push_str(&tok);
            }
        }
    }
}

/// Load a heads-up preflop chart set for `--preflop`, rejecting non-HU rulesets.
fn load_hu_charts(ruleset: &str) -> Result<PreflopCharts, String> {
    let charts =
        PreflopCharts::load(format!("data/preflop/{ruleset}")).map_err(|e| e.to_string())?;
    match charts.header.config.get("seats").and_then(|s| s.as_array()) {
        Some(seats) if seats.len() == 2 => Ok(charts),
        _ => Err(format!(
            "pot-odds --preflop supports heads-up rulesets only (cash-hu*, mtt-hu*); \
             {ruleset} is not heads-up"
        )),
    }
}

/// Entry point for `poker-trainer drill pot-odds`. Each flop spot — pot size,
/// hero's hand, and villain's range — is drawn from a solved heads-up preflop
/// equilibrium (`--preflop <ruleset>`, defaulting to `cash-hu89`); the call/fold
/// is scored against hero's equity vs villain's *whole range* at the node.
pub fn run_pot_odds_drill(preflop: Option<&str>) {
    let mut rng = rand::rng();
    let mut spots = 0u32;
    let mut correct = 0u32;

    let ruleset = preflop.unwrap_or(DEFAULT_POT_ODDS_RULESET);
    let charts = match load_hu_charts(ruleset) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("{e}");
            return;
        }
    };
    let stack_bb = charts
        .header
        .config
        .get("stack_bb")
        .and_then(|v| v.as_f64());

    println!("poker-trainer — pot-odds drill (flop only).");
    println!(
        "Spots drawn from {} preflop equilibrium.",
        charts.header.label
    );
    println!("Should you call? Type c)all or f)old. Empty line or q quits.\n");

    loop {
        let Some(spot) =
            (0..200).find_map(|_| sample_preflop_flop_spot(&charts, stack_bb, &mut rng))
        else {
            eprintln!(
                "  (this ruleset rarely reaches a non-all-in flop — \
                 try a deeper HU set like cash-hu89)"
            );
            break;
        };
        // Hero is a random seat; villain's range is the other seat's reach.
        let (hero, villain_reach) = if rng.random_range(0..2) == 0 {
            (spot.sb, spot.bb_reach)
        } else {
            (spot.bb, spot.sb_reach)
        };
        let flop = spot.flop;
        let pot = spot.pot;
        let line = preflop_line(&charts, &spot.path);

        let bet = pot * *BET_FRACTIONS.choose(&mut rng).unwrap();
        let req = required_equity(pot, bet);

        println!("Spot #{}", spots + 1);
        if !line.is_empty() {
            println!("  Preflop:   {}.", line.join(", "));
        }
        println!("  Your hand: {} {}", fmt(hero[0]), fmt(hero[1]));
        println!(
            "  Flop:      {} {} {}",
            fmt(flop[0]),
            fmt(flop[1]),
            fmt(flop[2])
        );
        println!("  Pot {pot:.1}bb. Villain bets {bet:.1}bb.");
        println!(
            "  Call {:.1} to win {:.1}  ->  need {:.0}% equity.",
            bet,
            pot + bet,
            req * 100.0
        );
        print!("  call or fold? > ");
        io::stdout().flush().unwrap();

        let mut input = String::new();
        if io::stdin().read_line(&mut input).unwrap() == 0 {
            break; // EOF (Ctrl-D)
        }
        let called = match input.trim().to_lowercase().as_str() {
            "c" | "call" => true,
            "f" | "fold" => false,
            "" | "q" | "quit" => break,
            _ => {
                println!("  (type c/call, f/fold, or q to quit)\n");
                continue; // re-deal, don't count
            }
        };

        let eq = preflop::equity_vs_reach(
            hero,
            &flop,
            &villain_reach,
            &mut rng,
            EQ_ITERS,
            EQ_VILLAIN_CAP,
        );
        let should_call = eq >= req;
        let right = called == should_call;
        spots += 1;
        if right {
            correct += 1;
        }

        println!(
            "  Equity vs villain's range: {:.1}%  (needed {:.1}%)",
            eq * 100.0,
            req * 100.0
        );
        println!(
            "  Best play: {} (call EV {:+.2}bb).  You said {} -> {}\n",
            if should_call { "CALL" } else { "FOLD" },
            call_ev(eq, pot, bet),
            if called { "call" } else { "fold" },
            if right { "correct" } else { "wrong" }
        );
    }

    report(correct, spots);
}

/// "UTG folds", "CO raises to 2.5bb", … — the action line leading to `path`,
/// rendered from the stored ancestor nodes (always present: export prunes
/// children before parents).
fn preflop_line(charts: &PreflopCharts, path: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut prefix = String::new();
    for tok in path.split('-').filter(|t| !t.is_empty()) {
        if let Some(node) = charts.node(&prefix) {
            if let Some(ai) = node
                .actions
                .iter()
                .position(|l| preflop::label_token(l) == tok)
            {
                let verb = match node.actions[ai].as_str() {
                    "Fold" => "folds".into(),
                    "Call" => "calls".into(),
                    "Check" => "checks".into(),
                    "All-in" => "jams".into(),
                    raise => raise.to_lowercase().replacen("raise", "raises", 1),
                };
                out.push(format!("{} {verb}", node.seat));
            }
        }
        if !prefix.is_empty() {
            prefix.push('-');
        }
        prefix.push_str(tok);
    }
    out
}

/// A uniformly random concrete combo of a 169-class.
fn deal_class_combo<R: RngExt>(class: usize, rng: &mut R) -> [Card; 2] {
    const SUITS: [char; 4] = ['s', 'h', 'd', 'c'];
    let name = preflop::class_name(class);
    let suited = name.len() == 3 && name.ends_with('s');
    let (s1, s2) = if suited {
        let s = SUITS[rng.random_range(0..4)];
        (s, s)
    } else {
        // pair or offsuit: two distinct suits
        let a = rng.random_range(0..4);
        let mut b = rng.random_range(0..3);
        if b >= a {
            b += 1;
        }
        (SUITS[a], SUITS[b])
    };
    let mut ch = name.chars();
    let card = |r: char, s: char| Card::try_from(format!("{r}{s}").as_str()).unwrap();
    [card(ch.next().unwrap(), s1), card(ch.next().unwrap(), s2)]
}

/// Entry point for `poker-trainer drill preflop` (design docs 04 + 07).
///
/// The solved chart library (`data/preflop/<ruleset>/`) is the answer key:
/// nodes are sampled by equilibrium reach, the hero's class by its arrival
/// probability, and the chosen action scores on EV loss through the same
/// [`NodeStrategy`] machinery as the postflop GTO drill.
pub fn run_preflop_drill(ruleset: &str) {
    let charts = match PreflopCharts::load(format!("data/preflop/{ruleset}")) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("{e}");
            return;
        }
    };
    // (node, per-class arrival) pool; nodes weighted by stored reach.
    let pool: Vec<(&PreflopNode, Vec<f32>)> = charts
        .nodes()
        .filter(|n| n.reach > 0.0)
        .filter_map(|n| Some((n, charts.class_reach(&n.path)?)))
        .collect();
    if pool.is_empty() {
        eprintln!("no drillable nodes in data/preflop/{ruleset}");
        return;
    }
    let reach_total: f32 = pool.iter().map(|(n, _)| n.reach).sum();

    let mut rng = rand::rng();
    let mut played = 0u32;
    let mut matched = 0u32; // picked an action GTO actually uses (>5%)
    let mut total_ev_loss = 0.0f32;
    let unit = charts.header.ev_unit.clone();
    println!(
        "poker-trainer — preflop drill: {} (EV in {unit}).",
        charts.header.label
    );
    println!("Pick the action by number. Empty line or q quits.\n");

    loop {
        // Sample a node ∝ reach, then the hero's class ∝ arrival × combos —
        // realistic spots, never a hand that can't get here.
        let mut roll = rng.random_range(0.0..reach_total);
        let (node, arrival) = pool
            .iter()
            .find(|(n, _)| {
                roll -= n.reach;
                roll < 0.0
            })
            .unwrap_or(&pool[0]);
        let weights: Vec<f32> = arrival
            .iter()
            .enumerate()
            .map(|(c, r)| r * preflop::class_combos(c) as f32)
            .collect();
        let wsum: f32 = weights.iter().sum();
        if wsum <= 0.0 {
            continue;
        }
        let mut roll = rng.random_range(0.0..wsum);
        let class = weights
            .iter()
            .position(|w| {
                roll -= w;
                roll < 0.0
            })
            .unwrap_or(preflop::CLASSES - 1);
        let hero = deal_class_combo(class, &mut rng);

        println!(
            "Spot #{} — you're the {}. Pot {:.1}bb, {:.1}bb to call.",
            played + 1,
            node.seat,
            node.pot_bb,
            node.to_call_bb
        );
        let line = preflop_line(&charts, &node.path);
        if !line.is_empty() {
            println!("  Action: {}.", line.join(", "));
        }
        println!("  Your hand: {} {}", fmt(hero[0]), fmt(hero[1]));
        let chosen = match read_pick(&node.actions) {
            Pick::At(i) => i,
            Pick::Quit => break,
            Pick::Retry => continue,
        };

        let freqs = node.freqs_for(class);
        played += 1;
        if freqs[chosen] >= stats::GTO_ACTION_FREQ {
            matched += 1;
        }

        // EV-loss scoring when the file carries EVs (always, for shipped
        // data); pure frequency scoring otherwise.
        let (best, ev_loss) = match node.strategy_for(class) {
            Some(ns) => {
                let (best, loss) = (ns.best(), ns.ev_loss(chosen));
                total_ev_loss += loss;
                print_gto_mix(&ns, best, &unit);
                println!(
                    "  You chose {} -> EV loss {:.2}{unit} (GTO plays it {:.1}%).\n",
                    ns.actions[chosen],
                    loss,
                    ns.frequencies[chosen] * 100.0
                );
                (best, Some(loss))
            }
            None => {
                let best = (0..freqs.len())
                    .max_by(|&a, &b| freqs[a].total_cmp(&freqs[b]))
                    .unwrap_or(0);
                println!(
                    "\n  GTO plays {} {:.1}% of the time here (most-played: {}).\n",
                    node.actions[chosen],
                    freqs[chosen] * 100.0,
                    node.actions[best]
                );
                (best, None)
            }
        };

        stats::record(&stats::StatRecord {
            formation: ruleset.into(),
            street: "preflop".into(),
            hand: format!("{}{}", hero[0], hero[1]),
            bucket: preflop::class_name(class),
            line,
            chosen: node.actions[chosen].clone(),
            best: node.actions[best].clone(),
            ev_loss,
            gto_freq: Some(freqs[chosen]),
            ..stats::StatRecord::new("preflop")
        });
    }

    if played > 0 {
        println!(
            "\nSession: {played} spots, {matched} on a GTO action ({:.0}%), avg EV loss {:.3}{unit}.",
            100.0 * matched as f64 / played as f64,
            total_ev_loss as f64 / played as f64
        );
    } else {
        println!("\nNo spots played.");
    }
}

/// Entry point for `poker-trainer drill gto` (Phase 1, plus Phase 3 live solve).
///
/// Pick a precomputed spot, deal the hero a hand from its solved range, present
/// the decision, and score the chosen action on EV loss vs. the equilibrium
/// mix. With a [`SolveRequest`] (`--board …`), live-solve that spot first.
pub fn run_gto_drill(req: Option<SolveRequest>) {
    let Some(provider) = resolve_provider(req) else {
        return;
    };
    let spots = provider.spots();

    let mut rng = rand::rng();
    let mut played = 0u32;
    let mut matched = 0u32; // picked an action GTO actually uses (>5%)
    let mut total_ev_loss = 0.0f32;

    println!("poker-trainer — GTO drill. Pick the action by number. Empty line or q quits.\n");

    loop {
        let spot = spots.choose(&mut rng).unwrap();
        let hand = spot.strategies.choose(&mut rng).unwrap();
        let ns = &hand.strategy;

        println!("Spot #{}: {}", played + 1, spot.label);
        println!("  Board: {}", fmt_hand_str(&spot.board.join("")));
        println!("  Pot {:.1}bb. {}.", spot.pot_bb, spot.villain_action);
        println!("  Your hand: {}", fmt_hand_str(&hand.hand));
        let chosen = match read_pick(&ns.actions) {
            Pick::At(i) => i,
            Pick::Quit => break,
            Pick::Retry => continue,
        };

        let best = ns.best();
        let ev_loss = ns.ev_loss(chosen);
        played += 1;
        total_ev_loss += ev_loss;
        if ns.frequencies[chosen] >= stats::GTO_ACTION_FREQ {
            matched += 1;
        }

        let (texture, bucket) = flop_context(&spot.board, &hand.hand);
        stats::record(&stats::StatRecord {
            formation: spot.formation().into(),
            flop: spot.board.join("").to_lowercase(),
            texture,
            street: "flop".into(),
            hand: hand.hand.clone(),
            bucket,
            line: vec![spot.villain_action.clone()],
            chosen: ns.actions[chosen].clone(),
            best: ns.actions[best].clone(),
            ev_loss: Some(ev_loss),
            gto_freq: Some(ns.frequencies[chosen]),
            ..stats::StatRecord::new("gto")
        });

        print_gto_mix(ns, best, "bb");
        println!(
            "  You chose {} -> EV loss {:.2}bb (GTO plays it {:.1}%).\n",
            ns.actions[chosen],
            ev_loss,
            ns.frequencies[chosen] * 100.0
        );
    }

    if played > 0 {
        println!(
            "\nSession: {played} spots, {matched} on a GTO action ({:.0}%), avg EV loss {:.3}bb.",
            100.0 * matched as f64 / played as f64,
            total_ev_loss as f64 / played as f64
        );
    } else {
        println!("\nNo spots played.");
    }
}

/// Pick the provider for a drill: live-solve when `req` is given (`--board`),
/// else the curated file library. Prints a hint and returns `None` on failure.
fn resolve_provider(req: Option<SolveRequest>) -> Option<Box<dyn SolutionProvider>> {
    match req {
        Some(req) => match LiveSolutionProvider::solve(&req, "data/solutions") {
            Ok(p) => Some(Box::new(p)),
            Err(e) => {
                eprintln!("Live solve failed: {e}");
                None
            }
        },
        None => load_provider().map(|p| Box::new(p) as Box<dyn SolutionProvider>),
    }
}

/// A generated reach-pruned table for this spot, if one exists under
/// `data/tables/<formation>/`. `None` (missing / too-new / malformed) means
/// live-solve instead.
fn load_table(req: &SolveRequest) -> Option<(PostflopTable, Option<crate::iso::SuitPerm>)> {
    let dir = std::path::Path::new("data/tables")
        .join(crate::postflop_table::formation_dir(&req.config.formation));
    find_table(&dir, &req.flop, &req.config.hash8())
}

/// The table serving `flop` in `dir`, plus the user→stored suit map when it
/// was stored under an isomorphic flop (design doc 08). Exact filename first
/// (today's behavior), then the canonical filename (the `all-iso-flops`
/// naming), then a directory scan — legacy tiers keep the manifest's card
/// order and suits in their stems, so any stem in the same isomorphism class
/// serves through the composed relabeling.
fn find_table(
    dir: &std::path::Path,
    flop: &str,
    hash: &str,
) -> Option<(PostflopTable, Option<crate::iso::SuitPerm>)> {
    if let Ok(t) = PostflopTable::load(dir, flop, hash) {
        return Some((t, None));
    }
    let (canon, to_canon) = crate::iso::canonical_flop(flop)?;
    if let Ok(t) = PostflopTable::load(dir, &canon, hash) {
        return Some((t, Some(to_canon).filter(|p| !p.is_identity())));
    }
    let suffix = format!("-{hash}.jsonl");
    for entry in std::fs::read_dir(dir).ok()? {
        let Ok(name) = entry.map(|e| e.file_name()) else {
            continue;
        };
        let Some(stem) = name.to_str().and_then(|n| n.strip_suffix(&suffix)) else {
            continue;
        };
        let Some((c, to_canon_file)) = crate::iso::canonical_flop(stem) else {
            continue;
        };
        if c != canon {
            continue;
        }
        let Ok(t) = PostflopTable::load(dir, stem, hash) else {
            continue;
        };
        // user→stored = (file→canonical)⁻¹ ∘ (user→canonical).
        let q = to_canon_file.inverse().compose(&to_canon);
        return Some((t, Some(q).filter(|p| !p.is_identity())));
    }
    None
}

/// Open a tree source for `req`: a disk-backed [`TableWalk`] when a reach-pruned
/// table is generated (off-path lines live-solve transparently), else a live
/// [`TreeSession`]. Both are `dyn TreeWalk`, so callers don't care which.
fn open_walk(req: &SolveRequest) -> io::Result<(Box<dyn TreeWalk>, TreeNode)> {
    if let Some((table, perm)) = load_table(req) {
        let via = match &perm {
            Some(_) => " via a suit-isomorphic stored flop",
            None => "",
        };
        eprintln!(
            "Using reach-pruned table for {}{via} ({} nodes) — off-path lines live-solve.",
            req.flop,
            table.len()
        );
        let (walk, root) = TableWalk::new(table, req.clone(), perm)?;
        Ok((Box::new(walk), root))
    } else {
        let (session, root) = TreeSession::start(req)?;
        Ok((Box::new(session), root))
    }
}

/// Load the precomputed solution library, or print a hint and return `None`.
fn load_provider() -> Option<FileSolutionProvider> {
    match FileSolutionProvider::load("data/solutions") {
        Ok(p) if !p.spots().is_empty() => Some(p),
        Ok(_) => {
            eprintln!("No solutions in data/solutions — run `cargo run -p solve-gen` first.");
            None
        }
        Err(e) => {
            eprintln!("Couldn't load data/solutions ({e}) — run `cargo run -p solve-gen` first.");
            None
        }
    }
}

/// Entry point for `poker-trainer table` — browse a solved spot's whole strategy
/// as a GTO-Wizard-style 13×13 grid. With `--board` it live-solves into a
/// [`TreeSession`] and walks the whole game tree (any line, any runout);
/// without it, it cycles the curated snapshot library exactly as before.
/// `line` (needs `--board`) descends that action line before the browser opens
/// — the replay handoff printed by `analyze`'s blunder list (design doc 05).
#[cfg(feature = "tui")]
pub fn run_table(req: Option<SolveRequest>, line: Option<String>, locks: Option<PathBuf>) {
    match req {
        Some(req) => {
            // Bail before the ~30 s solve if there's no terminal to draw on.
            if !std::io::stdout().is_terminal() {
                eprintln!(
                    "`table` draws an interactive color grid — run it in a terminal, not piped."
                );
                return;
            }
            // An existing --locks file is loaded and replayed; a missing one
            // is just where `S` will save.
            let mut loaded = match &locks {
                Some(p) if p.exists() => match crate::table::LockFile::load(p) {
                    Ok(f) => Some(f),
                    Err(e) => {
                        eprintln!("--locks: {e}");
                        return;
                    }
                },
                _ => None,
            };
            if let Some(f) = &loaded {
                if line.is_some() {
                    eprintln!("--line and a loaded --locks file both set a line; drop one.");
                    return;
                }
                let flop: String = f.board.iter().take(3).map(String::as_str).collect();
                if crate::solution::flop_key(&flop) != crate::solution::flop_key(&req.flop) {
                    eprintln!(
                        "--locks was saved on flop {flop}, not {} — refusing to apply it.",
                        req.flop
                    );
                    return;
                }
                if f.config_hash != req.config.hash8() {
                    eprintln!(
                        "warning: --locks was saved under config {}, this solve is {} — \
                         cells may not line up.",
                        f.config_hash,
                        req.config.hash8()
                    );
                }
            }
            match open_walk(&req) {
                Ok((mut walk, mut root)) => {
                    let spec = loaded
                        .as_ref()
                        .map(|f| f.line.join(","))
                        .filter(|s| !s.is_empty())
                        .or(line);
                    if let Some(spec) = spec {
                        match descend(walk.as_mut(), root, &spec) {
                            Ok(node) => root = node,
                            // Browse from wherever the line stopped matching —
                            // the solve is too expensive to throw away. Locks
                            // saved for a deeper node must not apply here.
                            Err(e) => {
                                eprintln!("line stopped early: {e}");
                                loaded = None;
                                match walk.node() {
                                    Ok(node) => root = node,
                                    Err(e) => {
                                        eprintln!("Tree session failed: {e}");
                                        return;
                                    }
                                }
                            }
                        }
                    }
                    crate::table::run_tree(
                        walk.as_mut(),
                        root,
                        crate::table::LockArgs {
                            path: locks,
                            loaded,
                            config_hash: req.config.hash8(),
                        },
                    )
                }
                Err(e) => eprintln!("Tree session failed: {e}"),
            }
        }
        None => {
            if line.is_some() {
                eprintln!("--line needs --board (a line only exists in a solved tree).");
                return;
            }
            if locks.is_some() {
                eprintln!("--locks needs --board (locks live in a solved tree).");
                return;
            }
            let Some(provider) = load_provider() else {
                return;
            };
            crate::table::run(provider.spots());
        }
    }
}

/// Descend a comma-separated label line, e.g. `"Check,Bet 2.0bb,deal 2c"` —
/// action labels as the tree prints them, `deal <card>` at chance nodes.
fn descend(session: &mut dyn TreeWalk, mut node: TreeNode, spec: &str) -> io::Result<TreeNode> {
    for step in spec.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        node = if let Some(card) = step.strip_prefix("deal ") {
            session.deal(card.trim())?
        } else {
            let i = node
                .actions
                .iter()
                .position(|a| a.eq_ignore_ascii_case(step))
                .ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!(
                            "no action {step:?} at this node (available: {})",
                            node.actions.join(", ")
                        ),
                    )
                })?;
            session.play(i)?
        };
    }
    Ok(node)
}

/// Entry point for `poker-trainer drill range` (Phase 2).
///
/// Pick one precomputed spot, bucket its whole range by made-hand strength, let
/// you assign an action per bucket, then score the full strategy: combo-weighted
/// EV loss and a per-bucket leak report.
pub fn run_range_drill(req: Option<SolveRequest>) {
    let Some(provider) = resolve_provider(req) else {
        return;
    };
    let spots = provider.spots();

    let mut rng = rand::rng();
    let spot = spots.choose(&mut rng).unwrap();

    let Some(flop) = parse_flop(&spot.board) else {
        eprintln!("Spot has an unparseable board ({:?}).", spot.board);
        return;
    };
    let actions = &spot.strategies[0].strategy.actions;

    println!("poker-trainer — range drill. Assign your whole range, one action per bucket.\n");
    println!("Spot: {}", spot.label);
    println!("  Board: {}", fmt_hand_str(&spot.board.join("")));
    println!("  Pot {:.1}bb. {}.", spot.pot_bb, spot.villain_action);
    println!("  Actions:");
    for (i, label) in actions.iter().enumerate() {
        println!("    {}) {}", i + 1, label);
    }

    // Villain's range = the opposite-position sibling's hero hands; sample it down
    // so the one-time equity pass stays sub-second.
    let villain: Vec<[Card; 2]> = villain_range(spots, spot)
        .sample(&mut rng, EQ_VILLAIN_CAP)
        .copied()
        .collect();
    print!("\n  Bucketing range by equity… ");
    io::stdout().flush().unwrap();
    let groups = group_by_subrange(spot, flop, &villain);
    println!("done.\n");

    // Assign one action per present sub-bucket (strong -> weak). q or EOF aborts.
    let mut chosen: BTreeMap<Subrange, usize> = BTreeMap::new();
    for (&sub, strats) in &groups {
        let pick = loop {
            let Some(input) = prompt(&format!("  {sub} ({} combos) action? > ", strats.len()))
            else {
                println!("\nAborted — nothing scored.");
                return;
            };
            if matches!(input.as_str(), "q" | "quit") {
                println!("\nAborted — nothing scored.");
                return;
            }
            match input
                .parse::<usize>()
                .ok()
                .filter(|n| (1..=actions.len()).contains(n))
            {
                Some(n) => break n - 1,
                None => println!("    (enter 1..{})", actions.len()),
            }
        };
        chosen.insert(sub, pick);
    }

    let leaks = score_buckets(&groups, &chosen);
    let (texture, _) = flop_context(&spot.board, "");
    for l in &leaks {
        let combos = l.combos.max(1) as f32;
        // A bucket assignment is one decision: `hand`/`best` stay empty (no
        // single combo or best action speaks for the whole bucket).
        stats::record(&stats::StatRecord {
            formation: spot.formation().into(),
            flop: spot.board.join("").to_lowercase(),
            texture: texture.clone(),
            street: "flop".into(),
            bucket: l.subrange.bucket.to_string(),
            line: vec![spot.villain_action.clone()],
            chosen: actions[l.action].clone(),
            ev_loss: Some(l.ev_loss / combos),
            gto_freq: Some(l.freq_sum / combos),
            ..stats::StatRecord::new("range")
        });
    }
    report_range(&leaks, actions);
}

/// One sub-bucket's contribution to the range score (all sums are over its combos).
struct BucketLeak {
    subrange: Subrange,
    combos: usize,
    action: usize,
    ev_loss: f32,
    freq_sum: f32, // summed GTO frequency of the chosen action
    matched: usize,
}

/// Score each sub-bucket's chosen action over its combos. Pure; sorted worst-first.
fn score_buckets(
    groups: &BTreeMap<Subrange, Vec<&NodeStrategy>>,
    chosen: &BTreeMap<Subrange, usize>,
) -> Vec<BucketLeak> {
    let mut leaks: Vec<BucketLeak> = groups
        .iter()
        .map(|(&subrange, strats)| {
            let action = chosen.get(&subrange).copied().unwrap_or(0);
            let mut leak = BucketLeak {
                subrange,
                combos: strats.len(),
                action,
                ev_loss: 0.0,
                freq_sum: 0.0,
                matched: 0,
            };
            for ns in strats {
                leak.ev_loss += ns.ev_loss(action);
                leak.freq_sum += ns.frequencies[action];
                if ns.frequencies[action] >= 0.05 {
                    leak.matched += 1;
                }
            }
            leak
        })
        .collect();
    // Worst-leaking bucket first, by EV lost *per combo* (severity, not just count).
    leaks.sort_by(|a, b| (b.ev_loss / b.combos as f32).total_cmp(&(a.ev_loss / a.combos as f32)));
    leaks
}

/// Print the per-sub-bucket leak report.
fn report_range(leaks: &[BucketLeak], actions: &[String]) {
    let combos: usize = leaks.iter().map(|l| l.combos).sum();
    if combos == 0 {
        println!("\nNo combos to score.");
        return;
    }
    let total_ev_loss: f32 = leaks.iter().map(|l| l.ev_loss).sum();
    let matched: usize = leaks.iter().map(|l| l.matched).sum();

    println!(
        "\nRange scored: {combos} combos in {} buckets.",
        leaks.len()
    );
    println!(
        "  Avg EV loss: {:.2}bb/combo  |  Accuracy: {:.0}% of combos on a GTO action.\n",
        total_ev_loss / combos as f32,
        100.0 * matched as f64 / combos as f64
    );
    println!(
        "  {:<12} {:>6}  {:<14} {:>9}  {:>12}",
        "bucket", "combos", "your action", "avg loss", "GTO plays it"
    );
    for l in leaks {
        println!(
            "  {:<12} {:>6}  {:<14} {:>6.2}bb  {:>11.0}%",
            l.subrange.to_string(),
            l.combos,
            actions[l.action],
            l.ev_loss / l.combos as f32,
            100.0 * (l.freq_sum / l.combos as f32)
        );
    }
}

/// Entry point for `poker-trainer drill hand` (Phase 5) — play full hands
/// (flop → river) against the equilibrium villain on a live tree session.
///
/// Villain is dealt a hidden hand from its range and plays the solved mix *for
/// that specific hand*, so runouts stay honest: a villain that check-raises
/// does so with the right part of its range (design doc 04). Hero decisions
/// are scored on EV loss but only revealed in the end-of-hand replay, so later
/// streets aren't played with the answer key open.
pub fn run_hand_drill(req: Option<SolveRequest>) {
    let Some(req) = req else {
        // ponytail: a tree session needs a solve config; sampling curated
        // spots arrives with the P6 library manifests.
        eprintln!("`drill hand` needs --board <flop> for now (e.g. --board Td9d6h).");
        return;
    };
    println!("poker-trainer — full-hand drill. Play flop to river; q quits.\n");
    if let Err(e) = hand_drill_loop(&req) {
        eprintln!("Tree session failed: {e}");
    }
}

fn hand_drill_loop(req: &SolveRequest) -> io::Result<()> {
    let (oop_seat, ip_seat) = crate::solution::formation(&req.config.formation)
        .map(|f| (f.oop_seat, f.ip_seat))
        .unwrap_or(("OOP", "IP"));
    let (mut walk, root) = open_walk(req)?;
    let oop_hands = root.hands.clone();
    // The node payload only carries the *acting* player's hands, and OOP acts
    // at the root — one step down any action is an IP node with IP's range.
    let ip_hands = walk.play(0)?.hands;

    let mut rng = rand::rng();
    let mut hands = 0u32;
    let mut decisions: Vec<HandDecision> = Vec::new(); // whole session

    loop {
        let root = walk.root()?;
        let hero_oop = rng.random_bool(0.5);
        let (hero_seat, hero_range, villain_range) = if hero_oop {
            ("oop", &oop_hands, &ip_hands)
        } else {
            ("ip", &ip_hands, &oop_hands)
        };
        // ponytail: uniform over combos — protocol v1 carries no range weights
        // and every shipped range is unweighted; put weights on the wire
        // before supporting weighted range strings here.
        let hero = hero_range
            .choose(&mut rng)
            .expect("a solved range is never empty")
            .clone();
        let live: Vec<String> = villain_range
            .iter()
            .filter(|v| !shares_card(v, &hero))
            .cloned()
            .collect();
        let Some(villain) = live.choose(&mut rng).cloned() else {
            eprintln!("Villain's whole range is blocked by your hand — ranges too narrow.");
            return Ok(());
        };

        hands += 1;
        println!(
            "Hand #{hands} — you're {} with {} on {}.",
            if hero_oop {
                format!("{oop_seat} (OOP)")
            } else {
                format!("{ip_seat} (IP)")
            },
            fmt_hand_str(&hero),
            fmt_hand_str(&root.board.join(""))
        );
        let outcome = play_hand(
            walk.as_mut(),
            root,
            hero_seat,
            &hero,
            &villain,
            &req.config.formation,
            &mut rng,
        )?;
        if !outcome.quit {
            replay(&outcome, &villain);
        }
        decisions.extend(outcome.decisions);
        if outcome.quit {
            break;
        }
        match prompt("\nEnter for the next hand, q to quit > ") {
            Some(s) if !matches!(s.as_str(), "q" | "quit") => println!(),
            _ => break,
        }
    }

    if decisions.is_empty() {
        println!("\nNo decisions scored.");
    } else {
        let n = decisions.len();
        let loss: f32 = decisions.iter().map(|d| d.ev_loss).sum();
        let matched = decisions
            .iter()
            .filter(|d| d.freq >= stats::GTO_ACTION_FREQ)
            .count();
        println!(
            "\nSession: {hands} hands, {n} decisions, {:.0}% on a GTO action, avg EV loss {:.3}bb.",
            100.0 * matched as f64 / n as f64,
            loss / n as f32
        );
    }
    Ok(())
}

/// One scored hero decision, kept for the end-of-hand replay.
struct HandDecision {
    street: &'static str,
    line: String,
    mix: String,
    chosen: String,
    best: String,
    freq: f32,
    ev_loss: f32,
}

struct HandOutcome {
    decisions: Vec<HandDecision>,
    quit: bool,
    final_pot: f32,
}

/// Walk one hand from the root to a terminal node: hero is prompted, villain
/// samples the equilibrium mix for its dealt hand, chance deals uniformly from
/// the cards neither player holds.
fn play_hand(
    session: &mut dyn TreeWalk,
    mut node: TreeNode,
    hero_seat: &str,
    hero: &str,
    villain: &str,
    formation: &str,
    rng: &mut impl RngExt,
) -> io::Result<HandOutcome> {
    let flop = node.board.clone();
    let mut out = HandOutcome {
        decisions: Vec::new(),
        quit: false,
        final_pot: node.pot_bb,
    };
    loop {
        out.final_pot = node.pot_bb;
        node = match node.player.as_str() {
            "terminal" => break,
            "chance" => {
                let live: Vec<&String> = node
                    .dealable
                    .iter()
                    .filter(|c| !blocks(hero, c) && !blocks(villain, c))
                    .collect();
                let card = (*live.choose(rng).expect("the deck can't run out")).clone();
                println!(
                    "  {}: {}",
                    street_name(node.board.len() + 1),
                    fmt_hand_str(&card)
                );
                session.deal(&card)?
            }
            p if p == hero_seat => {
                let Some((d, action)) = hero_decision(&node, hero) else {
                    out.quit = true;
                    break;
                };
                record_hand_decision(&flop, hero, formation, &node, &d);
                out.decisions.push(d);
                session.play(action)?
            }
            _ => {
                let vi = node
                    .hands
                    .iter()
                    .position(|h| h == villain)
                    .expect("villain's dealt hand is in its range");
                let weights: Vec<f32> = node.freqs.iter().map(|f| f[vi]).collect();
                let action = pick_weighted(&weights, rng.random());
                println!("  Villain: {}.", node.actions[action]);
                session.play(action)?
            }
        };
    }
    Ok(out)
}

/// Prompt the hero at a decision node, gto-drill style. `None` = quit/EOF; the
/// GTO mix is *not* revealed here — that waits for the replay.
fn hero_decision(node: &TreeNode, hero: &str) -> Option<(HandDecision, usize)> {
    let hi = node
        .hands
        .iter()
        .position(|h| h == hero)
        .expect("hero's dealt hand is in its range");
    let ns = NodeStrategy {
        actions: node.actions.clone(),
        frequencies: node.freqs.iter().map(|f| f[hi]).collect(),
        action_ev: node.evs.iter().map(|e| e[hi]).collect(),
    };
    let street = street_name(node.board.len());
    println!(
        "\n  [{street}] Board: {}   Pot {:.1}bb",
        fmt_hand_str(&node.board.join("")),
        node.pot_bb
    );
    if !node.line.is_empty() {
        println!("  Line: {}", node.line.join(" · "));
    }
    println!("  Your hand: {}", fmt_hand_str(hero));
    for (i, label) in ns.actions.iter().enumerate() {
        println!("    {}) {}", i + 1, label);
    }
    let chosen = loop {
        let input = prompt("  Your action? (number) > ")?;
        if matches!(input.as_str(), "q" | "quit") {
            return None;
        }
        match input
            .parse::<usize>()
            .ok()
            .filter(|n| (1..=ns.actions.len()).contains(n))
        {
            Some(n) => break n - 1,
            None => println!("    (enter 1..{}, or q to quit)", ns.actions.len()),
        }
    };
    Some((
        HandDecision {
            street,
            line: node.line.join(" · "),
            mix: fmt_mix(&ns),
            chosen: ns.actions[chosen].clone(),
            best: ns.actions[ns.best()].clone(),
            freq: ns.frequencies[chosen],
            ev_loss: ns.ev_loss(chosen),
        },
        chosen,
    ))
}

fn record_hand_decision(
    flop: &[String],
    hero: &str,
    formation: &str,
    node: &TreeNode,
    d: &HandDecision,
) {
    let (texture, bucket) = flop_context(flop, hero);
    stats::record(&stats::StatRecord {
        formation: formation.into(),
        flop: flop.join("").to_lowercase(),
        texture,
        street: d.street.into(),
        hand: hero.into(),
        bucket, // the *flop* bucket, whatever the street
        line: node.line.clone(),
        chosen: d.chosen.clone(),
        best: d.best.clone(),
        ev_loss: Some(d.ev_loss),
        gto_freq: Some(d.freq),
        ..stats::StatRecord::new("hand")
    });
}

/// Reveal villain and print one line per hero decision (design doc 04).
fn replay(out: &HandOutcome, villain: &str) {
    println!(
        "\n  Hand over — villain had {}. Final pot {:.1}bb.",
        fmt_hand_str(villain),
        out.final_pot
    );
    for d in &out.decisions {
        let line = if d.line.is_empty() { "(root)" } else { &d.line };
        println!(
            "    {:<5} {:<30} you: {:<14} loss {:>5.2}bb  GTO: {}",
            d.street, line, d.chosen, d.ev_loss, d.mix
        );
    }
}

/// "Check 55% / Bet 2.0bb 45%" — the actions GTO actually uses (>= 5%).
fn fmt_mix(ns: &NodeStrategy) -> String {
    ns.actions
        .iter()
        .zip(&ns.frequencies)
        .filter(|(_, &f)| f >= stats::GTO_ACTION_FREQ)
        .map(|(a, f)| format!("{a} {:.0}%", f * 100.0))
        .collect::<Vec<_>>()
        .join(" / ")
}

/// The 2-char card chunks of a packed hand string like `"AsKh"`.
fn card_chunks(s: &str) -> impl Iterator<Item = &str> {
    (0..s.len() / 2).map(move |i| &s[2 * i..2 * i + 2])
}

/// Do two packed hand strings share a card?
fn shares_card(a: &str, b: &str) -> bool {
    card_chunks(a).any(|c| blocks(b, c))
}

/// Does the packed hand string hold `card`?
fn blocks(hand: &str, card: &str) -> bool {
    card_chunks(hand).any(|c| c == card)
}

pub(crate) fn street_name(board_len: usize) -> &'static str {
    match board_len {
        0..=3 => "flop",
        4 => "turn",
        _ => "river",
    }
}

/// Print the "GTO mix:" block — each action's frequency and EV, with `<- best`
/// on the highest-EV action. `unit` is the EV suffix (`"bb"` or a chips label).
fn print_gto_mix(ns: &NodeStrategy, best: usize, unit: &str) {
    println!("\n  GTO mix:");
    for i in 0..ns.actions.len() {
        println!(
            "    {:<16} {:>5.1}%   EV {:+.2}{unit}{}",
            ns.actions[i],
            ns.frequencies[i] * 100.0,
            ns.action_ev[i],
            if i == best { "   <- best" } else { "" }
        );
    }
}

/// Index sampled from `weights` by a uniform `roll` in `[0, 1)`. Degenerate
/// all-zero weights (an unreachable node) fall back to action 0.
fn pick_weighted(weights: &[f32], roll: f32) -> usize {
    let total: f32 = weights.iter().sum();
    if total <= 0.0 {
        return 0;
    }
    let mut acc = 0.0;
    for (i, w) in weights.iter().enumerate() {
        acc += w;
        if roll * total < acc {
            return i;
        }
    }
    weights.len() - 1
}

/// `(texture, bucket)` strings for a history record; empty when cards don't
/// parse (records must never block a drill).
pub(crate) fn flop_context(board: &[String], hand: &str) -> (String, String) {
    let flop = board.get(..3).and_then(parse_flop);
    let texture = flop.map(texture_name).unwrap_or_default();
    let bucket = flop
        .zip(parse_hole(hand))
        .map(|(f, h)| eval::classify_hand(h, f).to_string())
        .unwrap_or_default();
    (texture, bucket)
}

/// A flop's one-word texture for grouping: paired beats suits.
fn texture_name(flop: [Card; 3]) -> String {
    texture::class(flop).into()
}

/// Parse a 3-card board (`["6h","9d","Td"]`) into a flop array.
fn parse_flop(board: &[String]) -> Option<[Card; 3]> {
    preflop::parse_cards(&board.join(""))?.try_into().ok()
}

/// Parse the hero's two hole cards from an `"AsKh"` string.
pub(crate) fn parse_hole(hand: &str) -> Option<[Card; 2]> {
    preflop::parse_cards(hand)?.try_into().ok()
}

/// Which equity half of a strength bucket a combo lands in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Half {
    Whole,  // bucket wasn't split (too small, or no villain range)
    Strong, // higher equity vs the villain's range
    Weak,   // lower equity
}

/// A strength bucket, optionally split by equity-vs-range. Sorts by bucket first
/// (strong -> weak), then Whole/Strong/Weak — so the report reads top to bottom.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct Subrange {
    bucket: Bucket,
    half: Half,
}

impl std::fmt::Display for Subrange {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.half {
            Half::Whole => write!(f, "{}", self.bucket),
            Half::Strong => write!(f, "{} ▲", self.bucket),
            Half::Weak => write!(f, "{} ▽", self.bucket),
        }
    }
}

/// The villain's range here = the opposite-position sibling node's hero hands.
///
/// ponytail: taken unweighted; for a defend node this includes the bettor's
/// checking hands too. Frequency-weight by bet freq if it ever shifts the tiers.
fn villain_range(spots: &[SolvedSpot], spot: &SolvedSpot) -> Vec<[Card; 2]> {
    spots
        .iter()
        .find(|s| s.board == spot.board && s.hero_oop != spot.hero_oop)
        .map(|s| {
            s.strategies
                .iter()
                .filter_map(|hs| parse_hole(&hs.hand))
                .collect()
        })
        .unwrap_or_default()
}

/// Split combos into (strong, weak) halves at the median equity; ties go strong.
/// Pure — equities are supplied, so it's testable without any Monte Carlo.
fn split_by_median(items: Vec<(f64, &NodeStrategy)>) -> (Vec<&NodeStrategy>, Vec<&NodeStrategy>) {
    let mut eqs: Vec<f64> = items.iter().map(|(e, _)| *e).collect();
    eqs.sort_by(f64::total_cmp);
    let median = eqs[eqs.len() / 2];
    let (strong, weak): (Vec<_>, Vec<_>) = items.into_iter().partition(|(e, _)| *e >= median);
    (
        strong.into_iter().map(|(_, ns)| ns).collect(),
        weak.into_iter().map(|(_, ns)| ns).collect(),
    )
}

/// Group a spot's combos into strength buckets, then split each big-enough bucket
/// by its members' equity vs the villain range. Skips unparseable hands.
fn group_by_subrange<'a>(
    spot: &'a SolvedSpot,
    flop: [Card; 3],
    villain: &[[Card; 2]],
) -> BTreeMap<Subrange, Vec<&'a NodeStrategy>> {
    // 1. classify each combo and measure its equity vs the villain range.
    let mut by_bucket: BTreeMap<Bucket, Vec<(f64, &NodeStrategy)>> = BTreeMap::new();
    for hs in &spot.strategies {
        if let Some(hole) = parse_hole(&hs.hand) {
            let eq = if villain.is_empty() {
                0.5
            } else {
                eval::equity_vs_range(hole, &flop, villain, EQ_ITERS)
            };
            by_bucket
                .entry(eval::classify_hand(hole, flop))
                .or_default()
                .push((eq, &hs.strategy));
        }
    }
    // 2. split each bucket at its median equity (small buckets stay whole).
    let mut groups: BTreeMap<Subrange, Vec<&NodeStrategy>> = BTreeMap::new();
    for (bucket, items) in by_bucket {
        if villain.is_empty() || items.len() < SPLIT_MIN {
            let combos = items.into_iter().map(|(_, ns)| ns).collect();
            groups.insert(
                Subrange {
                    bucket,
                    half: Half::Whole,
                },
                combos,
            );
        } else {
            let (strong, weak) = split_by_median(items);
            if !strong.is_empty() {
                groups.insert(
                    Subrange {
                        bucket,
                        half: Half::Strong,
                    },
                    strong,
                );
            }
            if !weak.is_empty() {
                groups.insert(
                    Subrange {
                        bucket,
                        half: Half::Weak,
                    },
                    weak,
                );
            }
        }
    }
    groups
}

/// Render a packed card string like `"Td9d6h"` or `"AsKh"` with suit glyphs.
pub(crate) fn fmt_hand_str(s: &str) -> String {
    s.as_bytes()
        .chunks(2)
        .filter_map(|c| std::str::from_utf8(c).ok())
        .filter_map(|cs| Card::try_from(cs).ok())
        .map(fmt)
        .collect::<Vec<_>>()
        .join(" ")
}

/// Prompt and read one trimmed, lowercased line. `None` on EOF (Ctrl-D).
fn prompt(msg: &str) -> Option<String> {
    print!("{msg}");
    io::stdout().flush().unwrap();
    let mut line = String::new();
    if io::stdin().read_line(&mut line).unwrap() == 0 {
        return None; // EOF
    }
    Some(line.trim().to_lowercase())
}

/// Outcome of a numbered-action prompt.
enum Pick {
    /// 0-based index into `actions`.
    At(usize),
    /// Empty line, `q`/`quit`, or EOF — end the drill.
    Quit,
    /// Non-numeric or out of range (hint already printed) — re-prompt.
    Retry,
}

/// Print `actions` numbered from 1, read a choice, and map it to a 0-based
/// index. Single source for the drill-loop input contract (see also `report`'s
/// `is_aggressive`: two copies of this diverged once).
fn read_pick(actions: &[String]) -> Pick {
    for (i, label) in actions.iter().enumerate() {
        println!("    {}) {}", i + 1, label);
    }
    let Some(input) = prompt("  Your action? (number) > ") else {
        return Pick::Quit;
    };
    if matches!(input.as_str(), "" | "q" | "quit") {
        return Pick::Quit;
    }
    match input
        .parse::<usize>()
        .ok()
        .filter(|n| (1..=actions.len()).contains(n))
    {
        Some(n) => Pick::At(n - 1),
        None => {
            println!("  (enter 1..{}, or q to quit)\n", actions.len());
            Pick::Retry
        }
    }
}

/// Print the end-of-session accuracy line.
fn report(correct: u32, spots: u32) {
    if spots > 0 {
        println!(
            "\nSession: {correct}/{spots} correct ({:.0}%).",
            100.0 * correct as f64 / spots as f64
        );
    } else {
        println!("\nNo spots played.");
    }
}

/// Card as e.g. `A♠` (nicer than rs_poker's default `As`).
fn fmt(c: Card) -> String {
    let suit = match c.suit {
        Suit::Spade => '♠',
        Suit::Heart => '♥',
        Suit::Diamond => '♦',
        Suit::Club => '♣',
    };
    format!("{}{}", char::from(c.value), suit)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Table lookup coverage (design doc 08): an exact stem hits untranslated;
    /// a reordered or isomorphic flop hits through the scan with the composed
    /// perm; different classes and hashes stay misses.
    #[test]
    fn find_table_serves_isomorphs_and_legacy_stems() {
        let config = crate::solution::SpotConfig {
            formation: "srp-btn-bb".into(),
            oop_range: "22".into(),
            ip_range: "33".into(),
            flop_sizes: "50%".into(),
            turn_sizes: "33%".into(),
            river_sizes: "33%".into(),
            stack_bb: 97.0,
            pot_bb: 6.0,
            rake_rate: 0.0,
            rake_cap_bb: 0.0,
        };
        let hash = config.hash8();
        let dir = std::env::temp_dir().join(format!("pt-find-table-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // Legacy-style stem: the manifest's card order, not the canonical form.
        std::fs::write(
            dir.join(format!("header-{hash}.json")),
            serde_json::json!({
                "version": 1, "formation": "srp-btn-bb", "config": config,
                "config_hash": hash,
                "generator": {"version": "0", "exploitability_bb": 0.02},
                "reach": 0.002,
            })
            .to_string(),
        )
        .unwrap();
        std::fs::write(
            dir.join(format!("td9d6h-{hash}.jsonl")),
            serde_json::json!({
                "reach": 1.0, "player": "oop",
                "board": ["6h", "9d", "Td"], "pot_bb": 6.0,
                "line": [], "actions": ["Check"],
                "hands": ["3d3c"], "freqs": [[1.0]], "evs": [[0.0]],
                "weights": [1.0], "equity": [0.5],
            })
            .to_string(),
        )
        .unwrap();

        // Exact stem: no translation.
        let (_, perm) = find_table(&dir, "Td9d6h", &hash).unwrap();
        assert!(perm.is_none());
        // Same cards reordered: found by scan, identity perm dropped.
        let (_, perm) = find_table(&dir, "9dTd6h", &hash).unwrap();
        assert!(perm.is_none());
        // Suit-isomorphic flop: found with the composed user→stored map.
        let (_, perm) = find_table(&dir, "Ts9s6h", &hash).unwrap();
        let q = perm.expect("isomorph needs a translation");
        assert_eq!(q.card("2s").as_deref(), Some("2d"), "user s → stored d");
        // A different isomorphism class and a different config both miss.
        assert!(find_table(&dir, "Th9d6c", &hash).is_none());
        assert!(find_table(&dir, "Td9d6h", "deadbeef").is_none());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn dealt_class_combos_map_back_to_their_class() {
        let mut rng = rand::rng();
        for class in [0, 1, 13, 84, 168] {
            for _ in 0..20 {
                let combo = deal_class_combo(class, &mut rng);
                assert_ne!(combo[0], combo[1]);
                assert_eq!(crate::preflop::class_index(combo), class, "class {class}");
            }
        }
    }

    #[test]
    fn preflop_lines_render_seats_and_verbs() {
        // A tiny synthetic chart: UTG folds, CO opens 2.5bb, BB to act.
        let dir = std::env::temp_dir().join(format!("pt-line-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let node = |path: &str, seat: &str, actions: &[&str]| {
            serde_json::json!({
                "path": path, "seat": seat, "pot_bb": 4.0, "to_call_bb": 2.5,
                "reach": 0.5,
                "actions": actions,
                "freqs": actions.iter().map(|_| vec![0.5f32; 169]).collect::<Vec<_>>(),
            })
            .to_string()
        };
        std::fs::write(
            dir.join("header.json"),
            serde_json::json!({
                "version": 1, "ruleset": "t", "label": "t", "config": {},
                "config_hash": "00000000", "ev_unit": "bb",
                "generator": {"version": "0", "traversals": 1, "seed": 1},
            })
            .to_string(),
        )
        .unwrap();
        std::fs::write(
            dir.join("starter.jsonl"),
            [
                node("", "UTG", &["Fold", "Raise to 2.5bb"]),
                node("f", "CO", &["Fold", "Raise to 2.5bb", "All-in"]),
                node("f-r2.5", "BB", &["Fold", "Call"]),
            ]
            .join("\n"),
        )
        .unwrap();

        let charts = PreflopCharts::load(&dir).unwrap();
        assert_eq!(
            preflop_line(&charts, "f-r2.5"),
            vec!["UTG folds", "CO raises to 2.5bb"]
        );
        assert_eq!(preflop_line(&charts, ""), Vec::<String>::new());

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn pot_odds_formula() {
        // Pot 10, bet 7: call 7 to win 17, break-even = 7 / (10 + 14) = 7/24.
        assert!((required_equity(10.0, 7.0) - 7.0 / 24.0).abs() < 1e-12);
    }

    #[test]
    fn hu_step_closes_only_on_call_or_check() {
        // Fold / all-in: no bettable flop.
        assert!(matches!(hu_step("r2.5", 5.0, 2.5, "Fold"), Step::Dead));
        assert!(matches!(hu_step("", 1.5, 0.5, "All-in"), Step::Dead));
        // BB checks behind a limp -> flop at the current pot (2bb).
        assert!(matches!(hu_step("c", 2.0, 0.0, "Check"), Step::Flop(p) if (p - 2.0).abs() < 1e-9));
        // SB's opening complete does NOT close (BB keeps the option).
        assert!(matches!(hu_step("", 1.5, 0.5, "Call"), Step::Continue));
        // BB calls a 2.5bb open -> flop of 5bb (pot 2.5 + to_call 2.5).
        assert!(
            matches!(hu_step("r2.5", 2.5, 2.5, "Call"), Step::Flop(p) if (p - 5.0).abs() < 1e-9)
        );
        // A raise continues.
        assert!(matches!(
            hu_step("", 1.5, 0.5, "Raise to 2.5bb"),
            Step::Continue
        ));
    }

    #[test]
    fn preflop_flop_spots_are_heads_up_consistent() {
        // The committed cash-hu89 starter is heads-up; a sampled flop spot must
        // have a real pot and seven distinct, non-colliding cards.
        let charts = load_hu_charts("cash-hu89").expect("cash-hu89 is heads-up");
        let mut rng = rand::rng();
        let spot = (0..10_000)
            .find_map(|_| sample_preflop_flop_spot(&charts, Some(89.0), &mut rng))
            .expect("cash-hu89 reaches a flop");
        assert!(spot.pot >= 2.0, "pot {} too small", spot.pot);
        let cards = [
            spot.sb[0],
            spot.sb[1],
            spot.bb[0],
            spot.bb[1],
            spot.flop[0],
            spot.flop[1],
            spot.flop[2],
        ];
        for (i, a) in cards.iter().enumerate() {
            for b in &cards[i + 1..] {
                assert_ne!(a, b, "duplicate card {a}");
            }
        }
        // A 6-max ruleset is rejected.
        assert!(load_hu_charts("cash89").is_err());
    }

    fn ns(freqs: Vec<f32>, evs: Vec<f32>) -> NodeStrategy {
        NodeStrategy {
            actions: vec!["Check".into(), "Bet".into()],
            frequencies: freqs,
            action_ev: evs,
        }
    }

    #[test]
    fn score_sums_ev_loss_over_combos_in_a_bucket() {
        // Two combos in one bucket, both assigned Check (action 0).
        // c1: EV [1.0, 3.0] -> ev_loss 2.0, freq[0]=0.00 (not a GTO action)
        // c2: EV [2.0, 5.0] -> ev_loss 3.0, freq[0]=0.10 (>= 5%, matched)
        let c1 = ns(vec![0.0, 1.0], vec![1.0, 3.0]);
        let c2 = ns(vec![0.10, 0.90], vec![2.0, 5.0]);
        let air = Subrange {
            bucket: Bucket::Air,
            half: Half::Whole,
        };
        let mut groups: BTreeMap<Subrange, Vec<&NodeStrategy>> = BTreeMap::new();
        groups.insert(air, vec![&c1, &c2]);
        let chosen = BTreeMap::from([(air, 0usize)]);

        let leaks = score_buckets(&groups, &chosen);
        assert_eq!(leaks.len(), 1);
        assert_eq!(leaks[0].combos, 2);
        assert!((leaks[0].ev_loss - 5.0).abs() < 1e-6); // 2.0 + 3.0, summed not single
        assert_eq!(leaks[0].matched, 1); // only c2 plays Check >= 5%
    }

    #[test]
    fn split_by_median_partitions_strong_and_weak() {
        // Four distinct equities -> median = eqs[2] = 0.6; >= median is strong.
        let (a, b, c, d) = (
            ns(vec![], vec![]),
            ns(vec![], vec![]),
            ns(vec![], vec![]),
            ns(vec![], vec![]),
        );
        let items = vec![(0.2, &a), (0.5, &b), (0.6, &c), (0.9, &d)];
        let (strong, weak) = split_by_median(items);
        assert_eq!(strong.len(), 2); // 0.6 and 0.9
        assert_eq!(weak.len(), 2); // 0.2 and 0.5

        // All-equal equities: median ties everything into the strong half.
        let (e, f) = (ns(vec![], vec![]), ns(vec![], vec![]));
        let (strong, weak) = split_by_median(vec![(0.5, &e), (0.5, &f)]);
        assert_eq!(strong.len(), 2);
        assert_eq!(weak.len(), 0);
    }

    #[test]
    fn pick_weighted_maps_rolls_to_cumulative_bins() {
        let w = [0.25, 0.75];
        assert_eq!(pick_weighted(&w, 0.0), 0);
        assert_eq!(pick_weighted(&w, 0.24), 0);
        assert_eq!(pick_weighted(&w, 0.25), 1);
        assert_eq!(pick_weighted(&w, 0.99), 1);
        // Unnormalized weights scale the same way.
        assert_eq!(pick_weighted(&[1.0, 3.0], 0.24), 0);
        assert_eq!(pick_weighted(&[1.0, 3.0], 0.26), 1);
        // Degenerate: all-zero falls back to 0; float tail lands on the last.
        assert_eq!(pick_weighted(&[0.0, 0.0], 0.5), 0);
        assert_eq!(pick_weighted(&[0.5, 0.5], 1.0), 1);
    }

    #[test]
    fn card_blocking_on_packed_hand_strings() {
        assert!(shares_card("AsKh", "KhQd"));
        assert!(!shares_card("AsKh", "QdQc"));
        assert!(blocks("AsKh", "As"));
        assert!(!blocks("AsKh", "Ad"));
    }

    #[test]
    fn street_names_follow_board_length() {
        assert_eq!(street_name(3), "flop");
        assert_eq!(street_name(4), "turn");
        assert_eq!(street_name(5), "river");
    }

    #[test]
    fn fmt_mix_hides_actions_gto_never_uses() {
        let ns = NodeStrategy {
            actions: vec!["Fold".into(), "Call".into(), "Raise".into()],
            frequencies: vec![0.02, 0.68, 0.30],
            action_ev: vec![0.0, 1.0, 1.0],
        };
        assert_eq!(fmt_mix(&ns), "Call 68% / Raise 30%");
    }

    /// End-to-end: `--line` descends a live tree by labels (analyze's replay
    /// handoff). Spawns a real (tiny) solve: `cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn descend_follows_labels_and_reports_bad_steps() {
        let req = SolveRequest {
            flop: "Td9d6h".into(),
            config: crate::solution::SpotConfig {
                formation: "srp-btn-bb".into(),
                oop_range: "AA,KK".into(),
                ip_range: "QQ,JJ".into(),
                flop_sizes: "50%".into(),
                turn_sizes: "33%".into(),
                river_sizes: "33%".into(),
                stack_bb: 97.0,
                pot_bb: 6.0,
                rake_rate: 0.0,
                rake_cap_bb: 0.0,
            },
        };
        let (mut session, root) = TreeSession::start(&req).unwrap();
        // Labels exactly as analyze records them: actions and `deal <card>`.
        let node = descend(&mut session, root, "Check, Bet 3.0bb, Call, deal 2c").unwrap();
        assert_eq!(node.board.last().unwrap(), "2c");
        assert_eq!(
            node.line,
            ["Check", "Bet 3.0bb", "Call", "deal 2c"].map(String::from)
        );

        // A label the node doesn't offer: a clear error, session still alive.
        let node2 = session.node().unwrap();
        let err = descend(&mut session, node2, "Bet 99.0bb").unwrap_err();
        assert!(err.to_string().contains("no action"));
        assert!(session.node().is_ok());
    }

    #[test]
    fn flop_context_classifies_and_survives_garbage() {
        let board: Vec<String> = ["Td", "9d", "6h"].map(String::from).to_vec();
        let (texture, bucket) = flop_context(&board, "Tc2s");
        assert_eq!(texture, "two-tone");
        assert_eq!(bucket, "TopPair");

        let paired: Vec<String> = ["8h", "8c", "3d"].map(String::from).to_vec();
        assert_eq!(flop_context(&paired, "").0, "paired");

        // Unparseable board/hand -> empty strings, never a panic.
        assert_eq!(
            flop_context(&["xx".into()], "??"),
            (String::new(), String::new())
        );
    }
}
