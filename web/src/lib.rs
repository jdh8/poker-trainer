//! Browser bindings for poker-trainer's pure-compute examples.
//!
//! Same shape as gin-rummy-web: each export returns its result as a JSON
//! string; JS parses and renders. All poker logic lives in the parent crate —
//! this crate only replaces terminal I/O. The GTO grid and preflop chart
//! pages need no wasm at all (they fetch the committed JSON directly).
//!
//! The internal `*_impl` functions return plain Rust types so the
//! `#[cfg(test)]` module runs natively (rlib), no browser needed.

use poker_trainer::{preflop, report};
use rs_poker::core::Card;
use serde::Serialize;
use wasm_bindgen::prelude::*;

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

// ---- villain-range equity (for the pot-odds drill) --------------------------

// ponytail: mirror the range-drill knobs in trainer.rs (EQ_ITERS/EQ_VILLAIN_CAP)
// rather than exporting them; a browser call must stay synchronous, so keep the
// per-combo runouts low and the sample capped.
const EQ_ITERS: u32 = 40;
const VILLAIN_CAP: usize = 150;

fn parse_cards<const N: usize>(s: &str) -> Result<[Card; N], String> {
    if s.len() != 2 * N {
        return Err(format!("{s:?} needs {N} cards"));
    }
    let v = (0..N)
        .map(|i| Card::try_from(&s[2 * i..2 * i + 2]).map_err(|_| format!("bad card in {s:?}")))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(v.try_into().unwrap())
}

fn equity_vs_reach_impl(hero: &str, flop: &str, reach: &[f32]) -> Result<f64, String> {
    if reach.len() != preflop::CLASSES {
        return Err(format!("reach needs {} class weights", preflop::CLASSES));
    }
    let mut rng = rand::rng();
    Ok(preflop::equity_vs_reach(
        parse_cards(hero)?,
        parse_cards(flop)?,
        reach,
        &mut rng,
        EQ_ITERS,
        VILLAIN_CAP,
    ))
}

/// Hero's equity vs the villain's *range* on a flop — what the pot-odds drill
/// scores against. That drill samples a spot in JS (walking the committed
/// preflop charts) and builds the villain seat's per-class reach — its 169
/// arrival weights in grid order — then asks wasm for the equity. `hero`/`flop`
/// are concatenated 2-char codes like `"AsKh"` (2 cards, then 3).
#[wasm_bindgen]
pub fn equity_vs_reach(hero: &str, flop: &str, reach: &[f32]) -> Result<f64, JsError> {
    equity_vs_reach_impl(hero, flop, reach).map_err(|e| JsError::new(&e))
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn equity_vs_reach_is_sane() {
        // Villain's range is pure 22; trip-less AA on a K Q 7 board dominates.
        let mut reach = vec![0.0f32; preflop::CLASSES];
        reach[preflop::class_index_of("22").unwrap()] = 1.0;
        let e = equity_vs_reach_impl("AhAs", "KdQc7h", &reach).unwrap();
        assert!(e > 0.8, "AA vs a pure-22 range should dominate: {e}");
        // Bad shapes are rejected.
        assert!(equity_vs_reach_impl("AhAs", "KdQc", &reach).is_err()); // board too short
        assert!(equity_vs_reach_impl("Zz", "KdQc7h", &reach).is_err()); // bad card code
        assert!(equity_vs_reach_impl("AhAs", "KdQc7h", &[0.0; 10]).is_err()); // wrong reach len
        // The export serializes.
        assert!(equity_vs_reach("AhAs", "KdQc7h", &reach).is_ok());
    }
}
