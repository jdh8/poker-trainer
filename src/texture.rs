//! Objective flop board-texture classification (Phase 0 — pure card logic).
//!
//! ponytail: only *objective* features (suit pattern, pairing, straightiness,
//! high card). Subjective "wet/dry equity-shift" scoring needs ranges — that's
//! Phase 2.

use rs_poker::core::{Card, Value};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SuitPattern {
    Monotone, // all three the same suit
    TwoTone,  // exactly two suits present
    Rainbow,  // three different suits
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Texture {
    pub suits: SuitPattern,
    pub paired: bool,    // two or three of a kind on the board
    pub straighty: bool, // three distinct ranks that fit in one 5-card straight window
    pub high: Value,     // highest card on the board
}

/// Classify a flop into its objective texture features.
pub fn classify(flop: [Card; 3]) -> Texture {
    let s = [flop[0].suit, flop[1].suit, flop[2].suit];
    let suits = if s[0] == s[1] && s[1] == s[2] {
        SuitPattern::Monotone
    } else if s[0] == s[1] || s[1] == s[2] || s[0] == s[2] {
        SuitPattern::TwoTone
    } else {
        SuitPattern::Rainbow
    };

    let r = [flop[0].value, flop[1].value, flop[2].value];
    let paired = r[0] == r[1] || r[1] == r[2] || r[0] == r[2];
    let high = *r.iter().max_by_key(|v| u8::from(**v)).unwrap();

    Texture {
        suits,
        paired,
        straighty: straighty(r),
        high,
    }
}

/// Three *distinct* ranks that fit inside a single 5-card straight window
/// (span ≤ 4), counting the ace as either high or low.
fn straighty(ranks: [Value; 3]) -> bool {
    let r = ranks.map(u8::from);
    if r[0] == r[1] || r[1] == r[2] || r[0] == r[2] {
        return false; // a pair can't be three-to-a-straight
    }
    let span = |v: [i16; 3]| v.iter().max().unwrap() - v.iter().min().unwrap();
    let high = r.map(|v| v as i16); // ace high (Ace = 12)
    let low = high.map(|v| if v == 12 { -1 } else { v }); // ace low (wheel)
    span(high).min(span(low)) <= 4
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
    fn two_tone_connected() {
        let t = classify(flop("Td", "9d", "6h"));
        assert_eq!(t.suits, SuitPattern::TwoTone);
        assert!(!t.paired);
        assert!(t.straighty); // T-9-6 spans 4
        assert_eq!(t.high, Value::Ten);
    }

    #[test]
    fn rainbow_dry() {
        let t = classify(flop("Kh", "7c", "2d"));
        assert_eq!(t.suits, SuitPattern::Rainbow);
        assert!(!t.paired);
        assert!(!t.straighty);
        assert_eq!(t.high, Value::King);
    }

    #[test]
    fn paired_rainbow() {
        let t = classify(flop("8h", "8c", "2d"));
        assert_eq!(t.suits, SuitPattern::Rainbow);
        assert!(t.paired);
        assert!(!t.straighty); // not three distinct ranks
    }

    #[test]
    fn monotone_broadway() {
        let t = classify(flop("As", "Ks", "Qs"));
        assert_eq!(t.suits, SuitPattern::Monotone);
        assert!(t.straighty);
        assert_eq!(t.high, Value::Ace);
    }

    #[test]
    fn wheel_is_straighty() {
        // A-3-4 with the ace playing low spans 4 (-1..3).
        assert!(classify(flop("Ah", "3c", "4d")).straighty);
    }
}
