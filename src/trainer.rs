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
use crate::texture::{self, SuitPattern};
use rand::seq::IndexedRandom;
use rs_poker::core::{Card, Deck, Suit};
use std::collections::BTreeMap;
use std::io::{self, IsTerminal, Write};

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
        if ns.frequencies[chosen] >= 0.05 {
            matched += 1;
        }

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

    report_range(&groups, &chosen, actions);
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
fn report_range(
    groups: &BTreeMap<Subrange, Vec<&NodeStrategy>>,
    chosen: &BTreeMap<Subrange, usize>,
    actions: &[String],
) {
    let leaks = score_buckets(groups, chosen);
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
    for l in &leaks {
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
}
