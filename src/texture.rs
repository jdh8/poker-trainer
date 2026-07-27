//! Objective flop board-texture classification (Phase 0 — pure card logic).
//!
//! ponytail: computes only the two objective features the grouping keys read —
//! suit pattern and pairing. Straightiness, high card, and subjective "wet/dry
//! equity-shift" scoring all need either ranges (Phase 2) or a consumer that
//! doesn't exist yet; add them back when something reads them.

use crate::solution::{SpotConfig, DEFAULT_FLOP_SIZES};
use rs_poker::core::Card;

/// A flop's one-word texture class for grouping (stats, reports): paired beats
/// suit pattern. Stable strings — they're grouping keys, not just display.
pub fn class(flop: [Card; 3]) -> &'static str {
    let r = [flop[0].value, flop[1].value, flop[2].value];
    if r[0] == r[1] || r[1] == r[2] || r[0] == r[2] {
        return "paired"; // pairing wins over any suit pattern
    }
    let s = [flop[0].suit, flop[1].suit, flop[2].suit];
    if s[0] == s[1] && s[1] == s[2] {
        "monotone"
    } else if s[0] == s[1] || s[1] == s[2] || s[0] == s[2] {
        "two-tone"
    } else {
        "rainbow"
    }
}

/// The texture class that keys **bet sizing**, read by BOTH the solve-gen
/// writer and the trainer lookup so their config hashes line up. Rank +
/// suit-pattern only (never a concrete suit) → suit-isomorphism-invariant,
/// which `iso.rs` relies on: a table is stored under the *canonical* flop but
/// looked up from the *raw* one, so `sizing_class(raw) == sizing_class(canon)`
/// must hold or the lookup misses.
///
/// ponytail: currently the same 4-way split as [`class`], but a separate
/// function so it can gain high-card / connectedness sub-classes at sizing
/// calibration without disturbing the stats grouping `class` feeds. Split a
/// class only once a reference solve shows it wants different sizing — measured,
/// not guessed.
pub fn sizing_class(flop: [Card; 3]) -> &'static str {
    class(flop)
}

/// The ≤2-size flop bet menu for a formation × [`sizing_class`] (`"a"` = the
/// all-in size token the solver parses). A `const` map baked from reference
/// solves.
///
/// Measured 2026-07-27 (srp-btn-bb deep + cash-hu55:c-x limped, root-EV probe):
/// `33%, 75%` is the EV-best ≤2 menu on *every* texture wherever it fits, so the
/// only cut that pays is **rainbow → a single `75%`** — it halves the OOM-monster
/// footprint (25.8→12.5 GB) at ≈EV-neutral (+2.6 mbb limped, −5.5 mbb deep, both
/// inside the ~13 mbb/side noise floor). A lone `75%` on a *wet* board loses
/// −17..−22 mbb, and all-in as a button never earns its slot above short-stack
/// SPR — so the other three textures keep the two-size default.
pub fn flop_sizes_for(_formation: &str, sizing_class: &str) -> &'static str {
    match sizing_class {
        "rainbow" => "75%",
        _ => DEFAULT_FLOP_SIZES,
    }
}

/// Derive `config.flop_sizes` from the flop's [`sizing_class`]. Deterministic in
/// `(formation, flop)`, so a table writer and the trainer reader independently
/// agree on the config hash. Three ways to leave it untouched:
///
/// - **Curated formations** (`srp-*`, `3bp-*` — no `:` in the id) are skipped so
///   their already-generated tables stay valid without a regen. Only grounded
///   `<ruleset>:<line>` tiers, which we regen, get the map.
/// - An explicit `--sizes` menu (`flop_sizes` already differs from the default)
///   wins — the escape hatch.
/// - A malformed flop string is left alone (the solve errors on it).
///
/// ponytail: grounded-only guard is what lets the regen stay grounded-tier-only.
/// Upgrade to apply the map on curated too: drop the `contains(':')` guard and
/// regen the curated tables.
pub fn specialize(config: &mut SpotConfig, flop: &str) {
    if !config.formation.contains(':') {
        return; // curated formation — keep its existing tables valid
    }
    if config.flop_sizes != DEFAULT_FLOP_SIZES {
        return; // an explicit --sizes override wins
    }
    if let Some(cards) = parse_flop(flop) {
        config.flop_sizes = flop_sizes_for(&config.formation, sizing_class(cards)).to_string();
    }
}

/// Parse a 6-char solver flop string (`"Td9d6h"`) into three cards.
fn parse_flop(s: &str) -> Option<[Card; 3]> {
    if s.len() != 6 {
        return None;
    }
    Some([
        s.get(0..2)?.try_into().ok()?,
        s.get(2..4)?.try_into().ok()?,
        s.get(4..6)?.try_into().ok()?,
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flop(a: &str, b: &str, c: &str) -> [Card; 3] {
        [
            a.try_into().unwrap(),
            b.try_into().unwrap(),
            c.try_into().unwrap(),
        ]
    }

    #[test]
    fn suit_patterns() {
        assert_eq!(class(flop("Td", "9d", "6h")), "two-tone");
        assert_eq!(class(flop("Kh", "7c", "2d")), "rainbow");
        assert_eq!(class(flop("As", "Ks", "Qs")), "monotone");
    }

    #[test]
    fn pairing_beats_suit_pattern() {
        assert_eq!(class(flop("8h", "8c", "2d")), "paired"); // paired rainbow
        assert_eq!(class(flop("8h", "8d", "2d")), "paired"); // paired two-tone
    }

    fn cfg(formation: &str, flop_sizes: &str) -> SpotConfig {
        SpotConfig {
            formation: formation.into(),
            oop_range: String::new(),
            ip_range: String::new(),
            flop_sizes: flop_sizes.into(),
            turn_sizes: String::new(),
            river_sizes: String::new(),
            stack_bb: 97.0,
            pot_bb: 6.0,
            rake_rate: 0.0,
            rake_cap_bb: 0.0,
        }
    }

    #[test]
    fn specialize_leaves_curated_formations_alone() {
        // Curated ids (no ':') keep the default so their existing tables stay
        // valid without a regen — even on a rainbow flop the map would degrade.
        let mut c = cfg("srp-btn-bb", DEFAULT_FLOP_SIZES);
        specialize(&mut c, "Kh7c2d"); // rainbow
        assert_eq!(c.flop_sizes, DEFAULT_FLOP_SIZES);
    }

    #[test]
    fn specialize_degrades_a_grounded_rainbow_to_a_single_75() {
        let mut c = cfg("cash-hu55:c-x", DEFAULT_FLOP_SIZES);
        specialize(&mut c, "Kh7c2d"); // rainbow → the one texture that cuts
        assert_eq!(c.flop_sizes, "75%");
    }

    #[test]
    fn specialize_keeps_grounded_wet_boards_at_the_default() {
        // Only rainbow cuts; two-tone/monotone/paired keep both sizes.
        for flop in ["Td9d6h", "Jh8h3h", "8h8c2d"] {
            let mut c = cfg("cash-hu55:c-x", DEFAULT_FLOP_SIZES);
            specialize(&mut c, flop);
            assert_eq!(c.flop_sizes, DEFAULT_FLOP_SIZES, "flop {flop}");
        }
    }

    #[test]
    fn specialize_respects_an_explicit_sizes_override() {
        // Grounded rainbow that would otherwise degrade — the --sizes hatch wins.
        let mut c = cfg("cash-hu55:c-x", "50%");
        specialize(&mut c, "Kh7c2d");
        assert_eq!(c.flop_sizes, "50%");
    }

    #[test]
    fn specialize_leaves_a_malformed_flop_alone() {
        let mut c = cfg("cash-hu55:c-x", DEFAULT_FLOP_SIZES);
        specialize(&mut c, "not-a-flop");
        assert_eq!(c.flop_sizes, DEFAULT_FLOP_SIZES);
    }

    #[test]
    fn sizing_class_is_suit_isomorphism_invariant() {
        // Constant across all 24 suit relabelings, or a table stored under the
        // canonical flop is never found from a raw one (iso.rs, design 08).
        use rs_poker::core::Suit;
        let suits = [Suit::Spade, Suit::Heart, Suit::Diamond, Suit::Club];
        let idx = |s: Suit| suits.iter().position(|&x| x == s).unwrap();
        let mut perms = Vec::new();
        for a in 0..4 {
            for b in 0..4 {
                for c in 0..4 {
                    for d in 0..4 {
                        if a != b && a != c && a != d && b != c && b != d && c != d {
                            perms.push([a, b, c, d]);
                        }
                    }
                }
            }
        }
        assert_eq!(perms.len(), 24);
        for base in [
            flop("Td", "9d", "6h"), // two-tone
            flop("Kh", "7c", "2d"), // rainbow
            flop("As", "Ks", "Qs"), // monotone
            flop("8h", "8c", "2d"), // paired
            flop("2s", "3s", "4s"), // monotone connected
        ] {
            let want = sizing_class(base);
            for p in &perms {
                let relabeled: [Card; 3] = std::array::from_fn(|i| Card {
                    value: base[i].value,
                    suit: suits[p[idx(base[i].suit)]],
                });
                assert_eq!(sizing_class(relabeled), want, "flop {base:?} perm {p:?}");
            }
        }
    }
}
