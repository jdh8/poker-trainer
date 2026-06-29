//! Hand evaluation & equity — a thin wrapper over the eval crate.
//!
//! Phase 0: `cargo add rs-poker` (and/or `pokers`) and implement these against
//! it. Centralize any card-index conversions between crates here.

use crate::board::Board;
use crate::range::Range;

/// Equity of a hero range vs. a villain range on a board, in [0.0, 1.0].
pub fn equity(_hero: &Range, _villain: &Range, _board: &Board) -> f32 {
    // TODO: delegate to rs-poker / pokers (Monte Carlo or exact enumeration).
    0.0
}
