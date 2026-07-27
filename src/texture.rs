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
/// solves — every arm currently returns the solver default, so [`specialize`]
/// is a no-op until the map is populated (design: postflop ≤2-button sizing).
pub fn flop_sizes_for(_formation: &str, _sizing_class: &str) -> &'static str {
    DEFAULT_FLOP_SIZES
}

/// Derive `config.flop_sizes` from the flop's [`sizing_class`] — unless the
/// caller already set an explicit menu (`flop_sizes` differs from the default,
/// i.e. the `--sizes` escape hatch was used). Deterministic in
/// `(formation, flop)`, so a writer and a reader independently agree on the
/// hash. A malformed flop string is left untouched (the solve errors on it).
///
/// ponytail: call sites are wired at sizing calibration (write side: solve-gen
/// `Spot` construction; read side: the tables path `open_walk`/`load_table` —
/// NOT the curated `data/solutions` provider, which stays at default sizes).
/// Landing it uncalled keeps this commit a pure no-op.
pub fn specialize(config: &mut SpotConfig, flop: &str) {
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

    fn cfg(flop_sizes: &str) -> SpotConfig {
        SpotConfig {
            formation: "srp-btn-bb".into(),
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
    fn specialize_is_a_noop_under_the_default_map() {
        // Every map arm returns the default today, so specialize must not move
        // flop_sizes (and hence the config hash) for any flop.
        let mut c = cfg(DEFAULT_FLOP_SIZES);
        specialize(&mut c, "Td9d6h");
        assert_eq!(c.flop_sizes, DEFAULT_FLOP_SIZES);
    }

    #[test]
    fn specialize_respects_an_explicit_sizes_override() {
        let mut c = cfg("50%"); // not the default → --sizes escape hatch
        specialize(&mut c, "Td9d6h");
        assert_eq!(c.flop_sizes, "50%");
    }

    #[test]
    fn specialize_leaves_a_malformed_flop_alone() {
        let mut c = cfg(DEFAULT_FLOP_SIZES);
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
