//! Objective flop board-texture classification (Phase 0 — pure card logic).
//!
//! ponytail: computes only the two objective features the grouping keys read —
//! suit pattern and pairing. Straightiness, high card, and subjective "wet/dry
//! equity-shift" scoring all need either ranges (Phase 2) or a consumer that
//! doesn't exist yet; add them back when something reads them.

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
}
