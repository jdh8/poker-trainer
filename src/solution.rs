//! The seam between "where GTO answers come from" and the rest of the trainer.
//!
//! Everything that needs a strategy asks a [`SolutionProvider`]. A file-backed
//! provider (precomputed sims) ships first; a live-solving provider backed by
//! `postflop-solver` can sit behind this same trait later — without touching
//! the trainer loop. Keeping that boundary here is also what keeps the AGPL
//! solver isolated to one crate when you add it.

use crate::board::Board;

/// A single node's optimal strategy: action frequencies + per-action EV.
#[derive(Debug, Clone)]
pub struct NodeStrategy {
    pub actions: Vec<Action>,
    /// Frequency for each action (parallel to `actions`), summing to ~1.0.
    pub frequencies: Vec<f32>,
    /// EV of each action in bb (parallel to `actions`).
    pub action_ev: Vec<f32>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    Fold,
    Check,
    Call,
    Bet(BetSize),
    Raise(BetSize),
}

#[derive(Debug, Clone, PartialEq)]
pub enum BetSize {
    /// Fraction of the pot, e.g. 0.6 == 60% pot.
    PotFraction(f32),
    /// Multiple of the bet being raised, e.g. 2.5x.
    Multiple(f32),
    AllIn,
}

/// Source of GTO strategies. Swap implementations without touching the trainer.
pub trait SolutionProvider {
    /// Strategy at the given spot for a specific hero hand `(rank, suit)` pair.
    fn strategy(&self, board: &Board, hero: [(u8, u8); 2]) -> Option<NodeStrategy>;
}

/// Phase 1: loads precomputed, serialized sims from disk.
pub struct FileSolutionProvider {
    // root dir of solved trees, an index keyed by (positions, pot type, depth, board), ...
}

impl SolutionProvider for FileSolutionProvider {
    fn strategy(&self, _board: &Board, _hero: [(u8, u8); 2]) -> Option<NodeStrategy> {
        // TODO (phase 1): look up & deserialize the matching solved node.
        None
    }
}
