//! Ground a postflop solve in a solved preflop line (design docs 07 → 08):
//! condense a flop-closing action line into exactly what the solver needs —
//! weighted arrival ranges, pot, effective stack, rake.
//!
//! [`ground`] is the single constructor behind the trainer's `--from`,
//! solve-gen's `--from`, and manifest `from =` runs. The config hash keys
//! data/solutions, data/tables, and the solve cache, so every path must build
//! the *identical* [`SpotConfig`] — shared code is what keeps that true, not
//! convention. A grounded config's formation id is the spec itself
//! (`"cash-hu55:r2.5-c"`); [`crate::postflop_table::formation_dir`] maps it
//! to an on-disk directory name.

use crate::preflop::{weighted_range_string, PreflopCharts};
use crate::solution::SpotConfig;
use std::path::Path;

/// Position-derived solve inputs for a flop-closing preflop `line`: both live
/// seats (OOP, IP), their per-class arrival reaches + weighted range strings,
/// the pot, effective stack, and rake — everything a postflop solve needs,
/// plus the reaches for hero/hand provenance.
#[derive(Debug)]
pub struct LineSpot {
    pub oop_seat: String,
    pub ip_seat: String,
    pub oop_reach: Vec<f32>,
    pub ip_reach: Vec<f32>,
    pub oop_range: String,
    pub ip_range: String,
    pub pot: f32,
    pub stack: f32,
    pub rake_rate: f32,
    pub rake_cap: f32,
}

/// Condense a solved preflop `line` into its two-player flop [`LineSpot`].
/// `Err` (user-facing) if the line isn't a clean two-player flop close or an
/// ancestor node was pruned — everything the caller should report and bail on.
pub fn derive_line_spot(charts: &PreflopCharts, line: &str) -> Result<LineSpot, String> {
    let (oop_seat, ip_seat) = charts.flop_seats(line).ok_or_else(|| {
        format!(
            "{line:?} isn't a two-player flop line (list lines with: export-range --ruleset <id>)"
        )
    })?;
    let pot = charts.flop_pot_bb(line).ok_or_else(|| {
        format!("{line:?} doesn't close to a flop (more action follows, or it's a fold/all-in)")
    })?;
    let reach = |seat: &str| {
        charts
            .seat_reach(line, seat)
            .ok_or_else(|| format!("{line:?} has a pruned/missing ancestor node"))
    };
    let oop_reach = reach(&oop_seat)?;
    let ip_reach = reach(&ip_seat)?;
    let oop_range = weighted_range_string(&oop_reach);
    let ip_range = weighted_range_string(&ip_reach);
    if oop_range.is_empty() || ip_range.is_empty() {
        return Err(format!(
            "{line:?} is unreachable for a live seat under the equilibrium"
        ));
    }
    let stack = charts
        .stack_bb()
        .ok_or_else(|| "ruleset config lacks stack_bb".to_string())?
        - charts.line_commitment_bb(line);
    let (rake_rate, rake_cap) = charts.rake();
    Ok(LineSpot {
        oop_seat,
        ip_seat,
        oop_reach,
        ip_reach,
        oop_range,
        ip_range,
        pot,
        stack,
        rake_rate,
        rake_cap,
    })
}

/// A grounded spot: the hash-bearing config plus the provenance around it.
#[derive(Debug)]
pub struct Grounded {
    /// The exact solve config — `formation` is the `<ruleset>:<line>` spec.
    pub config: SpotConfig,
    /// Seats, reaches, and stakes the config was derived from.
    pub spot: LineSpot,
    /// The ruleset's display label, for provenance lines.
    pub label: String,
}

/// Build the [`SpotConfig`] for a `<ruleset>:<line>` spec, loading the charts
/// under `<preflop_root>/<ruleset>`. `Err` is user-facing (bad spec, missing
/// ruleset, non-flop line).
pub fn ground(spec: &str, preflop_root: impl AsRef<Path>) -> Result<Grounded, String> {
    let (ruleset, line) = spec.split_once(':').ok_or_else(|| {
        format!("expected <ruleset>:<line>, e.g. cash-hu55:r2.5-c (got {spec:?})")
    })?;
    let charts =
        PreflopCharts::load(preflop_root.as_ref().join(ruleset)).map_err(|e| e.to_string())?;
    let spot = derive_line_spot(&charts, line)?;
    let config = SpotConfig {
        formation: spec.to_string(),
        oop_range: spot.oop_range.clone(),
        ip_range: spot.ip_range.clone(),
        // The solver-default sizes, same literals as `SpotConfig::for_formation`
        // — a grounded config replaces only what the preflop equilibrium fixes.
        flop_sizes: "33%, 75%".into(),
        turn_sizes: "33%".into(),
        river_sizes: "33%".into(),
        stack_bb: spot.stack,
        pot_bb: spot.pot,
        rake_rate: spot.rake_rate,
        rake_cap_bb: spot.rake_cap,
    };
    Ok(Grounded {
        config,
        spot,
        label: charts.header.label.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ground_builds_the_spec_named_config() {
        // Committed starter charts — the same spec the docs use everywhere.
        let g = ground("cash-hu55:r2.5-c", "data/preflop").unwrap();
        assert_eq!(g.config.formation, "cash-hu55:r2.5-c");
        assert!(!g.config.oop_range.is_empty());
        assert!(!g.config.ip_range.is_empty());
        assert_eq!(g.config.flop_sizes, "33%, 75%");
        assert!(g.config.pot_bb > 0.0);
        // Both raisers committed chips, so the effective stack shrank.
        assert!(g.config.stack_bb < 55.0);
        assert_eq!(g.spot.oop_seat, "BB");
        assert!(!g.label.is_empty());
        // Same spec twice = same hash (the alignment everything hangs on).
        let again = ground("cash-hu55:r2.5-c", "data/preflop").unwrap();
        assert_eq!(g.config.hash8(), again.config.hash8());
    }

    #[test]
    fn ground_rejects_bad_specs() {
        assert!(ground("no-colon", "data/preflop")
            .unwrap_err()
            .contains("expected <ruleset>:<line>"));
        assert!(ground("nope:r2.5-c", "data/preflop").is_err());
        // A fold line never reaches a flop.
        assert!(ground("cash-hu55:f", "data/preflop").is_err());
    }
}
