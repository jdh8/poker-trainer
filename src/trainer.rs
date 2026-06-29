//! The training loops.
//!
//! - `run_pot_odds_drill`: deal a hand + flop and a hidden villain hand, villain
//!   bets, you call or fold; scored against break-even pot odds using your true
//!   (Monte-Carlo) equity.
//! - `run_texture_drill`: deal a flop, you classify its objective texture.
//! - `run_gto_drill`: act vs. a precomputed solution; scored on EV loss (Phase 1).

use crate::eval;
use crate::solution::{FileSolutionProvider, SolutionProvider};
use crate::texture::{self, SuitPattern};
use rand::seq::IndexedRandom;
use rs_poker::core::{Card, Deck, Suit};
use std::io::{self, Write};

const POT: f64 = 10.0; // bb, fixed for now
const BET_FRACTIONS: [f64; 5] = [0.33, 0.5, 0.75, 1.0, 1.5];
const ITERS: u32 = 10_000;

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
        println!("  Flop:      {} {} {}", fmt(flop[0]), fmt(flop[1]), fmt(flop[2]));
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
            if t.straighty { "straighty" } else { "disconnected" },
            char::from(t.high),
            if right { "correct" } else { "wrong" }
        );
    }

    report(correct, spots);
}

/// Entry point for `poker-trainer drill gto` (Phase 1).
///
/// Pick a precomputed spot, deal the hero a hand from its solved range, present
/// the decision, and score the chosen action on EV loss vs. the equilibrium mix.
pub fn run_gto_drill() {
    let provider = match FileSolutionProvider::load("data/solutions") {
        Ok(p) if !p.spots().is_empty() => p,
        Ok(_) => {
            eprintln!("No solutions in data/solutions — run `cargo run -p solve-gen` first.");
            return;
        }
        Err(e) => {
            eprintln!("Couldn't load data/solutions ({e}) — run `cargo run -p solve-gen` first.");
            return;
        }
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
        let Some(chosen) = input.parse::<usize>().ok().filter(|n| (1..=ns.actions.len()).contains(n))
        else {
            println!("  (enter 1..{}, or q to quit)\n", ns.actions.len());
            continue;
        };
        let chosen = chosen - 1;

        let best = ns.best();
        let ev_loss = (ns.action_ev[best] - ns.action_ev[chosen]).max(0.0);
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

/// Render a packed card string like `"Td9d6h"` or `"AsKh"` with suit glyphs.
fn fmt_hand_str(s: &str) -> String {
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
    use super::required_equity;

    #[test]
    fn pot_odds_formula() {
        // Pot 10, bet 7: call 7 to win 17, break-even = 7 / (10 + 14) = 7/24.
        assert!((required_equity(10.0, 7.0) - 7.0 / 24.0).abs() < 1e-12);
    }
}
