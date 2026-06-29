//! Board representation: the community cards.

use rs_poker::core::Card;

/// Community cards after the flop. Turn/river are dealt as the hand progresses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Board {
    pub flop: [Card; 3],
    pub turn: Option<Card>,
    pub river: Option<Card>,
}
