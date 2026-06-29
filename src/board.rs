//! Board representation: the community cards.

/// Community cards after the flop. Turn/river are dealt as the hand progresses.
#[derive(Debug, Clone)]
pub struct Board {
    pub flop: [Card; 3],
    pub turn: Option<Card>,
    pub river: Option<Card>,
}

/// Placeholder card type. In phase 0, run `cargo add rs-poker` and replace this
/// with `rs_poker::core::Card`, then delete this definition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Card {
    pub rank: u8, // 2..=14 (J=11, Q=12, K=13, A=14)
    pub suit: u8, // 0..=3
}
