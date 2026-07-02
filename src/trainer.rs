//! The training loops.
//!
//! - `run_pot_odds_drill`: deal a hand + flop and a hidden villain hand, villain
//!   bets, you call or fold; scored against break-even pot odds using your true
//!   (Monte-Carlo) equity.
//! - `run_texture_drill`: deal a flop, you classify its objective texture.
//! - `run_gto_drill`: act vs. a precomputed solution; scored on EV loss (Phase 1).

use crate::eval::{self, Bucket};
use crate::solution::{
    FileSolutionProvider, LiveSolutionProvider, NodeStrategy, SolutionProvider, SolveRequest,
    SolvedSpot,
};
use crate::stats;
use crate::texture::{self, SuitPattern};
use crate::tree::{TreeNode, TreeSession};
use rand::seq::IndexedRandom;
use rand::RngExt;
use rs_poker::core::{Card, Deck, Suit};
use std::collections::BTreeMap;
use std::io::{self, IsTerminal, Write};

/// A spot's formation id for history records; pre-v2 files carry no config and
/// were all generated as BTN-vs-BB SRPs.
fn spot_formation(spot: &SolvedSpot) -> String {
    spot.config
        .as_ref()
        .map(|c| c.formation.clone())
        .unwrap_or_else(|| "srp-btn-bb".into())
}

const POT: f64 = 10.0; // bb, fixed for now
const BET_FRACTIONS: [f64; 5] = [0.33, 0.5, 0.75, 1.0, 1.5];
const ITERS: u32 = 10_000;

// Range-drill equity sub-bucketing knobs.
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

/// Entry point for `poker-trainer drill pot-odds`.
pub fn run_pot_odds_drill() {
    let mut rng = rand::rng();
    let mut spots = 0u32;
    let mut correct = 0u32;

    println!("poker-trainer — pot-odds drill (flop only).");
    println!("Should you call? Type c)all or f)old. Empty line or q quits.\n");

    loop {
        // Deal everything from one fresh deck so nothing collides.
        let mut deck = Deck::default();
        let mut draw = || deck.deal(&mut rng).unwrap();
        let hero = [draw(), draw()];
        let villain = [draw(), draw()];
        let flop = [draw(), draw(), draw()];

        let bet = POT * *BET_FRACTIONS.choose(&mut rng).unwrap();
        let req = required_equity(POT, bet);

        println!("Spot #{}", spots + 1);
        println!("  Your hand: {} {}", fmt(hero[0]), fmt(hero[1]));
        println!(
            "  Flop:      {} {} {}",
            fmt(flop[0]),
            fmt(flop[1]),
            fmt(flop[2])
        );
        println!("  Pot {POT:.0}bb. Villain bets {bet:.1}bb.");
        println!(
            "  Call {:.1} to win {:.1}  ->  need {:.0}% equity.",
            bet,
            POT + bet,
            req * 100.0
        );
        print!("  call or fold? > ");
        io::stdout().flush().unwrap();

        let mut line = String::new();
        if io::stdin().read_line(&mut line).unwrap() == 0 {
            break; // EOF (Ctrl-D)
        }
        let called = match line.trim().to_lowercase().as_str() {
            "c" | "call" => true,
            "f" | "fold" => false,
            "" | "q" | "quit" => break,
            _ => {
                println!("  (type c/call, f/fold, or q to quit)\n");
                continue; // re-deal, don't count
            }
        };

        let eq = eval::equity(hero, villain, flop, ITERS);
        let should_call = eq >= req;
        let right = called == should_call;
        spots += 1;
        if right {
            correct += 1;
        }

        println!(
            "  True equity: {:.1}%  (needed {:.1}%)",
            eq * 100.0,
            req * 100.0
        );
        println!("  Villain had: {} {}", fmt(villain[0]), fmt(villain[1]));
        println!(
            "  Best play: {} (call EV {:+.2}bb).  You said {} -> {}\n",
            if should_call { "CALL" } else { "FOLD" },
            call_ev(eq, POT, bet),
            if called { "call" } else { "fold" },
            if right { "correct" } else { "wrong" }
        );
    }

    report(correct, spots);
}

/// Entry point for `poker-trainer drill texture`.
///
/// Deal a flop; you name its suit pattern and whether it's paired. Both must be
/// right to score the spot. We reveal the full objective texture either way.
pub fn run_texture_drill() {
    let mut rng = rand::rng();
    let mut spots = 0u32;
    let mut correct = 0u32;

    println!("poker-trainer — board-texture drill.");
    println!("Name the suit pattern and whether the flop is paired. Empty line or q quits.\n");

    loop {
        let mut deck = Deck::default();
        let mut draw = || deck.deal(&mut rng).unwrap();
        let flop = [draw(), draw(), draw()];
        let t = texture::classify(flop);

        println!("Spot #{}", spots + 1);
        println!("  Flop: {} {} {}", fmt(flop[0]), fmt(flop[1]), fmt(flop[2]));

        let Some(suit_ans) = prompt("  Suit pattern? r)ainbow t)wo-tone m)onotone > ") else {
            break;
        };
        let guessed_suits = match suit_ans.as_str() {
            "r" | "rainbow" => SuitPattern::Rainbow,
            "t" | "two-tone" | "twotone" => SuitPattern::TwoTone,
            "m" | "monotone" => SuitPattern::Monotone,
            "" | "q" | "quit" => break,
            _ => {
                println!("  (type r/t/m, or q to quit)\n");
                continue;
            }
        };

        let Some(pair_ans) = prompt("  Paired? y/n > ") else {
            break;
        };
        let guessed_paired = match pair_ans.as_str() {
            "y" | "yes" => true,
            "n" | "no" => false,
            "" | "q" | "quit" => break,
            _ => {
                println!("  (type y/n, or q to quit)\n");
                continue;
            }
        };

        let right = guessed_suits == t.suits && guessed_paired == t.paired;
        spots += 1;
        if right {
            correct += 1;
        }

        println!(
            "  Texture: {} pattern, {}, {}, high card {}.  -> {}\n",
            match t.suits {
                SuitPattern::Rainbow => "rainbow",
                SuitPattern::TwoTone => "two-tone",
                SuitPattern::Monotone => "monotone",
            },
            if t.paired { "paired" } else { "unpaired" },
            if t.straighty {
                "straighty"
            } else {
                "disconnected"
            },
            char::from(t.high),
            if right { "correct" } else { "wrong" }
        );
    }

    report(correct, spots);
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
        for (i, label) in ns.actions.iter().enumerate() {
            println!("    {}) {}", i + 1, label);
        }

        let Some(input) = prompt("  Your action? (number) > ") else {
            break;
        };
        if matches!(input.as_str(), "" | "q" | "quit") {
            break;
        }
        let Some(chosen) = input
            .parse::<usize>()
            .ok()
            .filter(|n| (1..=ns.actions.len()).contains(n))
        else {
            println!("  (enter 1..{}, or q to quit)\n", ns.actions.len());
            continue;
        };
        let chosen = chosen - 1;

        let best = ns.best();
        let ev_loss = ns.ev_loss(chosen);
        played += 1;
        total_ev_loss += ev_loss;
        if ns.frequencies[chosen] >= stats::GTO_ACTION_FREQ {
            matched += 1;
        }

        let (texture, bucket) = flop_context(&spot.board, &hand.hand);
        stats::record(&stats::StatRecord {
            formation: spot_formation(spot),
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

        println!("\n  GTO mix:");
        for i in 0..ns.actions.len() {
            println!(
                "    {:<14} {:>5.1}%   EV {:+.2}bb{}",
                ns.actions[i],
                ns.frequencies[i] * 100.0,
                ns.action_ev[i],
                if i == best { "   <- best" } else { "" }
            );
        }
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
pub fn run_table(req: Option<SolveRequest>) {
    match req {
        Some(req) => {
            // Bail before the ~30 s solve if there's no terminal to draw on.
            if !std::io::stdout().is_terminal() {
                eprintln!(
                    "`table` draws an interactive color grid — run it in a terminal, not piped."
                );
                return;
            }
            match crate::tree::TreeSession::start(&req) {
                Ok((session, root)) => crate::table::run_tree(session, root),
                Err(e) => eprintln!("Tree session failed: {e}"),
            }
        }
        None => {
            let Some(provider) = load_provider() else {
                return;
            };
            crate::table::run(provider.spots());
        }
    }
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
            formation: spot_formation(spot),
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
    let (mut session, root) = TreeSession::start(req)?;
    let oop_hands = root.hands.clone();
    // The node payload only carries the *acting* player's hands, and OOP acts
    // at the root — one step down any action is an IP node with IP's range.
    let ip_hands = session.play(0)?.hands;

    let mut rng = rand::rng();
    let mut hands = 0u32;
    let mut decisions: Vec<HandDecision> = Vec::new(); // whole session

    loop {
        let root = session.root()?;
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
            &mut session,
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
    session: &mut TreeSession,
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

fn street_name(board_len: usize) -> &'static str {
    match board_len {
        0..=3 => "flop",
        4 => "turn",
        _ => "river",
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
fn flop_context(board: &[String], hand: &str) -> (String, String) {
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
    let t = texture::classify(flop);
    if t.paired {
        return "paired".into();
    }
    match t.suits {
        SuitPattern::Monotone => "monotone",
        SuitPattern::TwoTone => "two-tone",
        SuitPattern::Rainbow => "rainbow",
    }
    .into()
}

/// Parse a 3-card board (`["6h","9d","Td"]`) into a flop array.
fn parse_flop(board: &[String]) -> Option<[Card; 3]> {
    parse_cards(&board.join(""))?.try_into().ok()
}

/// Parse the hero's two hole cards from an `"AsKh"` string.
pub(crate) fn parse_hole(hand: &str) -> Option<[Card; 2]> {
    parse_cards(hand)?.try_into().ok()
}

/// Parse a packed card string (`"6h9dTd"`) into cards; `None` if any chunk fails.
fn parse_cards(s: &str) -> Option<Vec<Card>> {
    s.as_bytes()
        .chunks(2)
        .map(|c| {
            std::str::from_utf8(c)
                .ok()
                .and_then(|cs| Card::try_from(cs).ok())
        })
        .collect()
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
                eval::equity_vs_range(hole, flop, villain, EQ_ITERS)
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

    #[test]
    fn pot_odds_formula() {
        // Pot 10, bet 7: call 7 to win 17, break-even = 7 / (10 + 14) = 7/24.
        assert!((required_equity(10.0, 7.0) - 7.0 / 24.0).abs() < 1e-12);
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
