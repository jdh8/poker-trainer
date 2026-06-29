//! The training loop: a pot-odds call/fold drill (flop only).
//!
//! Each spot deals you a hand + flop and a hidden random villain hand, villain
//! bets, and you decide call or fold. We score your choice against the
//! break-even pot odds using your true (Monte-Carlo) equity, and track accuracy.

use crate::eval;
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

/// Entry point for `poker-trainer drill`.
pub fn run_drill() {
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
