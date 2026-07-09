//! Browser bindings for poker-trainer's pure-compute examples.
//!
//! Same shape as gin-rummy-web: each export returns its result as a JSON
//! string; JS parses and renders. All poker logic lives in the parent crate —
//! this crate only replaces terminal I/O. The GTO grid and preflop chart
//! pages need no wasm at all (they fetch the committed JSON directly).
//!
//! The internal `*_impl` functions return plain Rust types so the
//! `#[cfg(test)]` module runs natively (rlib), no browser needed.

use poker_trainer::{eval, report};
use rand::seq::IndexedRandom;
use rs_poker::core::{Card, Deck, Suit};
use serde::Serialize;
use wasm_bindgen::prelude::*;

/// A card as a 2-char code like `"As"` — JS maps suits to symbols and colors.
fn card_str(c: Card) -> String {
    let suit = match c.suit {
        Suit::Spade => 's',
        Suit::Heart => 'h',
        Suit::Diamond => 'd',
        Suit::Club => 'c',
    };
    format!("{}{}", char::from(c.value), suit)
}

// ---- equity calculator ------------------------------------------------------

#[derive(Serialize)]
struct EquityReport {
    board: String,
    oop_mean: f64,
    oop_n: usize,
    ip_n: usize,
    oop_bins: [usize; 10],
    ip_bins: [usize; 10],
}

fn equity_report_impl(oop: &str, ip: &str, board: &str) -> Result<EquityReport, String> {
    let flop =
        report::parse_flop(board).ok_or("board needs exactly 3 cards on the flop, e.g. Td9d6h")?;
    let oop = report::live_combos(report::parse_range(oop)?, flop);
    let ip = report::live_combos(report::parse_range(ip)?, flop);
    if oop.is_empty() || ip.is_empty() {
        return Err("a range is empty after removing board cards — nothing to match up".into());
    }
    let oop_eqs = report::combo_equities(&oop, &ip, flop);
    let ip_eqs = report::combo_equities(&ip, &oop, flop);
    Ok(EquityReport {
        board: board.into(),
        oop_mean: oop_eqs.iter().sum::<f64>() / oop_eqs.len() as f64,
        oop_n: oop.len(),
        ip_n: ip.len(),
        oop_bins: report::histogram(&oop_eqs),
        ip_bins: report::histogram(&ip_eqs),
    })
}

/// Range-vs-range equity on a flop — the web port of `poker-trainer equity`.
#[wasm_bindgen]
pub fn equity_report(oop: &str, ip: &str, board: &str) -> Result<String, JsError> {
    equity_report_impl(oop, ip, board)
        .map(|r| serde_json::to_string(&r).unwrap())
        .map_err(|e| JsError::new(&e))
}

// ---- pot-odds drill ---------------------------------------------------------

// ponytail: these three mirror private one-liners in trainer.rs rather than
// exporting trainer internals; the drill loop there owns them.
const POT: f64 = 10.0;
const BET_FRACTIONS: [f64; 5] = [0.33, 0.5, 0.75, 1.0, 1.5];
const ITERS: u32 = 10_000;

#[derive(Serialize)]
struct PotOddsSpot {
    hero: [String; 2],
    villain: [String; 2],
    flop: [String; 3],
    pot: f64,
    bet: f64,
    required: f64,
    equity: f64,
    should_call: bool,
    call_ev: f64,
}

fn deal_pot_odds_impl() -> PotOddsSpot {
    let mut rng = rand::rng();
    let mut deck = Deck::default();
    let mut draw = || deck.deal(&mut rng).unwrap();
    let hero = [draw(), draw()];
    let villain = [draw(), draw()];
    let flop = [draw(), draw(), draw()];
    let bet = POT * *BET_FRACTIONS.choose(&mut rng).unwrap();
    let required = bet / (POT + 2.0 * bet);
    let equity = eval::equity(hero, villain, flop, ITERS);
    PotOddsSpot {
        hero: hero.map(card_str),
        villain: villain.map(card_str),
        flop: flop.map(card_str),
        pot: POT,
        bet,
        required,
        equity,
        should_call: equity >= required,
        call_ev: equity * (POT + bet) - (1.0 - equity) * bet,
    }
}

/// One pot-odds spot, answer precomputed — the web port of `drill pot-odds`.
/// JS hides `villain`/`equity`/`should_call`/`call_ev` until the user answers.
#[wasm_bindgen]
pub fn deal_pot_odds() -> String {
    serde_json::to_string(&deal_pot_odds_impl()).unwrap()
}

// ---- single-hand equity (for the preflop-sourced pot-odds drill) ------------

fn equity_of_impl(hero: &str, villain: &str, board: &str) -> Result<f64, String> {
    fn cards<const N: usize>(s: &str) -> Result<[Card; N], String> {
        if s.len() != 2 * N {
            return Err(format!("{s:?} needs {N} cards"));
        }
        let v = (0..N)
            .map(|i| Card::try_from(&s[2 * i..2 * i + 2]).map_err(|_| format!("bad card in {s:?}")))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(v.try_into().unwrap())
    }
    Ok(eval::equity(
        cards(hero)?,
        cards(villain)?,
        cards(board)?,
        ITERS,
    ))
}

/// Hero-vs-villain equity on a flop — the specific-hand Monte-Carlo the
/// preflop-sourced pot-odds drill needs. That drill samples the cards in JS
/// (walking the committed preflop charts), then asks wasm for the equity. Args
/// are concatenated 2-char codes like `"AsKh"`: hero/villain 2 cards, board 3.
#[wasm_bindgen]
pub fn equity_of(hero: &str, villain: &str, board: &str) -> Result<f64, JsError> {
    equity_of_impl(hero, villain, board).map_err(|e| JsError::new(&e))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn equity_report_favors_aces() {
        let r = equity_report_impl("AA", "KK", "Td9d6h").unwrap();
        assert!(r.oop_mean > 0.7, "AA vs KK mean was {}", r.oop_mean);
        assert_eq!(r.oop_bins.iter().sum::<usize>(), r.oop_n);
        assert_eq!(r.ip_bins.iter().sum::<usize>(), r.ip_n);
        // The export serializes cleanly.
        assert!(equity_report("AA", "KK", "Td9d6h").is_ok());
    }

    #[test]
    fn equity_report_rejects_bad_input() {
        assert!(equity_report_impl("AA", "KK", "Td9d").is_err());
        assert!(equity_report_impl("not a range", "KK", "Td9d6h").is_err());
        // TT on a Txx flop leaves one live combo, but a range that is 100%
        // board cards would be empty; the closest legal probe is fine to skip.
    }

    #[test]
    fn equity_of_is_sane() {
        // Set of aces crushes a pair of deuces on a dry board.
        let e = equity_of_impl("AhAs", "2c2d", "AdKhQs").unwrap();
        assert!(e > 0.9, "trip aces vs 22 should crush: {e}");
        assert!(equity_of_impl("AhAs", "2c2d", "AdKh").is_err()); // board too short
        assert!(equity_of_impl("Zz", "2c2d", "AdKhQs").is_err()); // bad card code
        assert!(equity_of("AhAs", "2c2d", "AdKhQs").is_ok()); // export serializes
    }

    #[test]
    fn pot_odds_spot_is_consistent() {
        let s = deal_pot_odds_impl();
        let cards: HashSet<&String> = s
            .hero
            .iter()
            .chain(s.villain.iter())
            .chain(s.flop.iter())
            .collect();
        assert_eq!(cards.len(), 7, "cards collide");
        assert!((s.required - s.bet / (s.pot + 2.0 * s.bet)).abs() < 1e-12);
        assert_eq!(s.should_call, s.equity >= s.required);
        assert!(s.equity > 0.0 && s.equity < 1.0);
    }
}
