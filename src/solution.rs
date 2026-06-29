//! The seam between "where GTO answers come from" and the rest of the trainer.
//!
//! A [`SolvedSpot`] is one precomputed decision node: the spot's setup plus, for
//! every hero hand, the equilibrium action mix and per-action EV. The trainer
//! reads these; the `solve-gen` crate (AGPL, isolated) produces them. Keeping
//! the file format here — not postflop-solver's own tree format — is what keeps
//! the solver out of the shipped trainer binary.

use serde::{Deserialize, Serialize};
use std::fs;
use std::io;
use std::path::Path;

/// A single decision node's equilibrium strategy.
///
/// `actions`, `frequencies`, and `action_ev` are parallel. Action labels are
/// pre-rendered strings (e.g. `"Check"`, `"Bet 2.0bb"`) — v1 only displays and
/// scores them, so there's no structured action type to carry.
/// EVs are in big blinds.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeStrategy {
    pub actions: Vec<String>,
    /// Frequency of each action, summing to ~1.0.
    pub frequencies: Vec<f32>,
    /// EV of each action in bb.
    pub action_ev: Vec<f32>,
}

impl NodeStrategy {
    /// Index of the highest-EV action (the GTO-best single action).
    pub fn best(&self) -> usize {
        self.action_ev
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.total_cmp(b.1))
            .map(|(i, _)| i)
            .unwrap_or(0)
    }
}

/// One precomputed decision node and the strategy for every hero hand at it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SolvedSpot {
    /// Human label, e.g. "SRP BTN vs BB — c-bet on Td9d6h".
    pub label: String,
    /// Board so far, as rs_poker card strings: `["Td", "9d", "6h"]`.
    pub board: Vec<String>,
    /// Pot at the hero's decision, in bb.
    pub pot_bb: f32,
    /// True if the hero acts out of position.
    pub hero_oop: bool,
    /// How we reached the hero's decision, e.g. "Villain bets 2.0bb (33% pot)".
    pub villain_action: String,
    /// Per-hero-hand strategies.
    pub strategies: Vec<HandStrategy>,
}

/// The equilibrium strategy for one specific hero holding.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HandStrategy {
    /// Hero's two hole cards, as one rs_poker string, e.g. `"AsKh"`.
    pub hand: String,
    pub strategy: NodeStrategy,
}

/// Source of precomputed GTO solutions. A live-solving provider can implement
/// this same trait later without touching the trainer (README's key seam).
pub trait SolutionProvider {
    fn spots(&self) -> &[SolvedSpot];
}

/// Loads precomputed [`SolvedSpot`]s from `data/solutions/*.json`.
pub struct FileSolutionProvider {
    spots: Vec<SolvedSpot>,
}

impl FileSolutionProvider {
    /// Load every `*.json` solution file in `dir`.
    pub fn load(dir: impl AsRef<Path>) -> io::Result<Self> {
        let mut spots = Vec::new();
        // ponytail: O(n) linear load over a curated handful of files; index by
        // board key only if the library outgrows hand-curation.
        for entry in fs::read_dir(dir)? {
            let path = entry?.path();
            if path.extension().is_some_and(|e| e == "json") {
                let spot = serde_json::from_str(&fs::read_to_string(&path)?)
                    .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
                spots.push(spot);
            }
        }
        Ok(Self { spots })
    }
}

impl SolutionProvider for FileSolutionProvider {
    fn spots(&self) -> &[SolvedSpot] {
        &self.spots
    }
}
