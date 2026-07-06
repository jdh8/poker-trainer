//! Browser bindings for poker-trainer's pure-compute examples.
//!
//! Same shape as gin-rummy-web: each export returns its result as a JSON
//! string; JS parses and renders. All poker logic lives in the parent crate —
//! this crate only replaces terminal I/O. The GTO grid page needs no wasm at
//! all (it fetches the solution JSON directly).
//!
//! The internal `*_impl` functions return plain Rust types so the
//! `#[cfg(test)]` module runs natively (rlib), no browser needed.

use poker_trainer::solution::{Formation, FORMATIONS};
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

// ---- preflop charts ---------------------------------------------------------

/// The committed preflop chart text, embedded so the browser needs no fetch
/// (there is no filesystem in wasm). One solver range string per file.
fn range_text(id: &str, seat_file: &str) -> &'static str {
    match (id, seat_file) {
        ("srp-btn-bb", "oop") => include_str!("../../data/ranges/srp-btn-bb/oop.txt"),
        ("srp-btn-bb", "ip") => include_str!("../../data/ranges/srp-btn-bb/ip.txt"),
        ("srp-co-bb", "oop") => include_str!("../../data/ranges/srp-co-bb/oop.txt"),
        ("srp-co-bb", "ip") => include_str!("../../data/ranges/srp-co-bb/ip.txt"),
        ("srp-sb-bb", "oop") => include_str!("../../data/ranges/srp-sb-bb/oop.txt"),
        ("srp-sb-bb", "ip") => include_str!("../../data/ranges/srp-sb-bb/ip.txt"),
        ("3bp-bb-btn", "oop") => include_str!("../../data/ranges/3bp-bb-btn/oop.txt"),
        ("3bp-bb-btn", "ip") => include_str!("../../data/ranges/3bp-bb-btn/ip.txt"),
        ("3bp-btn-co", "oop") => include_str!("../../data/ranges/3bp-btn-co/oop.txt"),
        ("3bp-btn-co", "ip") => include_str!("../../data/ranges/3bp-btn-co/ip.txt"),
        _ => "",
    }
}

/// Canonical 169-cell class of a combo: `"AA"`, `"AKs"`, `"AKo"` — the same
/// mapping `app.js`'s grid uses, so an in-range combo lights the right cell.
fn combo_class(h: [Card; 2]) -> String {
    const RANKS: &str = "AKQJT98765432";
    let (a, b) = (char::from(h[0].value), char::from(h[1].value));
    if a == b {
        return format!("{a}{b}");
    }
    let (hi, lo) = if RANKS.find(a) < RANKS.find(b) {
        (a, b)
    } else {
        (b, a)
    };
    format!("{hi}{lo}{}", if h[0].suit == h[1].suit { 's' } else { 'o' })
}

#[derive(Serialize)]
struct PreflopChart {
    id: &'static str,
    label: &'static str,
    seat: String,
    action: &'static str,
    /// Raise/3-bet (aggressive color) vs. call the open/3-bet (passive color).
    aggressive: bool,
    /// The 169-cell class names this seat plays; every other cell folds.
    classes: Vec<String>,
}

/// The in-range 169-cell classes for one seat's binary chart.
fn chart_classes(id: &str, seat_file: &str) -> Vec<String> {
    let combos = report::parse_range(range_text(id, seat_file).trim()).unwrap_or_default();
    let mut classes: Vec<String> = combos.into_iter().map(combo_class).collect();
    classes.sort();
    classes.dedup();
    classes
}

/// The two charts a formation answers: the aggressor's open/3-bet range and the
/// defender's call range. Seat→file follows `trainer::preflop_spots` (the file
/// name is postflop position, so the opener isn't always `ip`).
fn charts_for(f: &Formation) -> Vec<PreflopChart> {
    let mut it = f.id.split('-');
    let kind = it.next().unwrap_or("");
    let seats = [
        it.next().unwrap_or("").to_uppercase(),
        it.next().unwrap_or("").to_uppercase(),
    ];
    let (act0, act1) = if kind == "srp" {
        ("opens", "defends")
    } else {
        ("3-bets", "calls 3-bet")
    };
    seats
        .into_iter()
        .zip([(act0, true), (act1, false)])
        .map(|(seat, (action, aggressive))| {
            let file = if seat.eq_ignore_ascii_case(f.oop_seat) {
                "oop"
            } else {
                "ip"
            };
            PreflopChart {
                id: f.id,
                label: f.label,
                classes: chart_classes(f.id, file),
                seat,
                action,
                aggressive,
            }
        })
        .collect()
}

fn preflop_charts_impl() -> Vec<PreflopChart> {
    FORMATIONS.iter().flat_map(charts_for).collect()
}

/// Every formation's preflop charts as JSON — the browsable web view of the
/// `data/ranges/` chart files that `drill preflop` quizzes on.
#[wasm_bindgen]
pub fn preflop_charts() -> String {
    serde_json::to_string(&preflop_charts_impl()).unwrap()
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

    #[test]
    fn preflop_charts_cover_every_formation() {
        let charts = preflop_charts_impl();
        assert_eq!(charts.len(), FORMATIONS.len() * 2);
        for c in &charts {
            // Non-empty and every class is a valid grid cell (pair "AA" = 2
            // chars, suited/offsuit "AKs" = 3), unique after dedup.
            assert!(!c.classes.is_empty(), "empty chart for {} {}", c.id, c.seat);
            assert!(c.classes.iter().all(|s| (2..=3).contains(&s.len())));
            assert_eq!(
                c.classes.iter().collect::<HashSet<_>>().len(),
                c.classes.len()
            );
        }
    }
}
