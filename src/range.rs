//! Hand-range representation and parsing (e.g. "22+, AKs, ATo+, AA@50").
//!
//! Don't reimplement range-string parsing — phase 0, back this with
//! `pokers::HandRange::from_strings` or rs-poker's range parsing.

/// A weighted range of starting hands (combo -> weight in [0.0, 1.0]).
#[derive(Debug, Default, Clone)]
pub struct Range {
    // Fill in once the eval crate is wired up.
}
