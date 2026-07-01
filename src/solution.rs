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
use std::process::Command;

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

    /// EV given up by taking `chosen` instead of the best action, in bb (>= 0).
    pub fn ev_loss(&self, chosen: usize) -> f32 {
        (self.action_ev[self.best()] - self.action_ev[chosen]).max(0.0)
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

/// What to live-solve for a custom spot. Only `flop` is required; `None` fields
/// let `solve-gen` apply its own defaults. Everything is an opaque string we
/// forward — the trainer never parses ranges or bet sizes (that needs the
/// solver), which is what keeps it unlinked from postflop-solver. Serde because
/// this is also the `config` of a tree-session `op:solve` (see `tree`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SolveRequest {
    pub flop: String,
    pub oop: Option<String>,
    pub ip: Option<String>,
    pub sizes: Option<String>,
    pub stack: Option<f32>,
    pub pot: Option<f32>,
}

impl SolveRequest {
    pub fn new(flop: impl Into<String>) -> Self {
        Self {
            flop: flop.into(),
            oop: None,
            ip: None,
            sizes: None,
            stack: None,
            pot: None,
        }
    }

    /// True if any field overrides solve-gen's defaults. The on-disk cache key
    /// is the flop alone, so a custom request forces a re-solve even when a
    /// file for this flop already exists.
    pub fn is_custom(&self) -> bool {
        self.oop.is_some()
            || self.ip.is_some()
            || self.sizes.is_some()
            || self.stack.is_some()
            || self.pot.is_some()
    }
}

/// Live-solving provider: shells out to the `solve-gen` binary (the only thing
/// that links the solver), which writes `SolvedSpot` JSON into the solution
/// dir; we then load just the spots for the requested flop. Delivers on the
/// README's "a live-solving provider can implement this same trait".
pub struct LiveSolutionProvider {
    spots: Vec<SolvedSpot>,
}

impl LiveSolutionProvider {
    /// Solve `req` into `dir` (unless a non-custom request is already cached
    /// there), then load the spots whose board matches the requested flop.
    pub fn solve(req: &SolveRequest, dir: impl AsRef<Path>) -> io::Result<Self> {
        let dir = dir.as_ref();
        let stem = req.flop.to_lowercase();
        let cached = dir.join(format!("{stem}-ip.json")).exists();
        if !cached || req.is_custom() {
            eprintln!(
                "Solving {} — postflop-solver, expect ~30 s and ~1 GB RAM…",
                req.flop
            );
            run_solve_gen(req, dir)?;
        }

        let key = flop_key(&req.flop);
        let spots: Vec<SolvedSpot> = FileSolutionProvider::load(dir)?
            .spots
            .into_iter()
            .filter(|s| flop_key(&s.board.join("")) == key)
            .collect();
        if spots.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("no solved spots for flop {} in {}", req.flop, dir.display()),
            ));
        }
        Ok(Self { spots })
    }
}

impl SolutionProvider for LiveSolutionProvider {
    fn spots(&self) -> &[SolvedSpot] {
        &self.spots
    }
}

/// Split a flop string like `"Td9d6h"` (or a joined board) into sorted
/// lowercase cards — an order-independent identity, since the file stem keeps
/// the user's card order but the board field is solver-sorted.
fn flop_key(flop: &str) -> Vec<String> {
    let mut cards: Vec<String> = flop
        .as_bytes()
        .chunks(2)
        .map(|c| String::from_utf8_lossy(c).to_lowercase())
        .collect();
    cards.sort();
    cards
}

/// The `solve …` argv passed to solve-gen (program excluded) — pure, so it's
/// unit-testable without spawning anything.
fn solve_gen_args(req: &SolveRequest, out_dir: &Path) -> Vec<String> {
    let mut a = vec!["solve".into(), "--flop".into(), req.flop.clone()];
    let mut opt = |flag: &str, val: &str| {
        a.push(flag.into());
        a.push(val.into());
    };
    if let Some(v) = &req.oop {
        opt("--oop", v);
    }
    if let Some(v) = &req.ip {
        opt("--ip", v);
    }
    if let Some(v) = &req.sizes {
        opt("--sizes", v);
    }
    if let Some(v) = req.stack {
        opt("--stack", &v.to_string());
    }
    if let Some(v) = req.pot {
        opt("--pot", &v.to_string());
    }
    opt("--out", &out_dir.to_string_lossy());
    a
}

/// The command to run solve-gen with `args`: a prebuilt binary via
/// `POKER_TRAINER_SOLVE_GEN`, else `cargo run -p solve-gen` for the dev
/// workspace. Stderr is inherited so solve progress shows live.
// ponytail: cargo-run shim is fine in-tree; point the env var at a packaged
// solve-gen binary when shipping a standalone trainer.
pub(crate) fn solve_gen_command(args: &[String]) -> Command {
    match std::env::var_os("POKER_TRAINER_SOLVE_GEN") {
        Some(bin) => {
            let mut c = Command::new(bin);
            c.args(args);
            c
        }
        None => {
            let mut c = Command::new("cargo");
            c.args(["run", "-p", "solve-gen", "--release", "--quiet", "--"]);
            c.args(args);
            c
        }
    }
}

/// Spawn solve-gen, inheriting stdout/stderr so its progress + any range/size
/// parse error show live.
fn run_solve_gen(req: &SolveRequest, out_dir: &Path) -> io::Result<()> {
    let status = solve_gen_command(&solve_gen_args(req, out_dir)).status()?;
    if !status.success() {
        return Err(io::Error::new(
            io::ErrorKind::Other,
            format!("solve-gen failed ({status}) — see its output above"),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_spot() -> SolvedSpot {
        SolvedSpot {
            label: "test".into(),
            board: vec!["Td".into(), "9d".into(), "6h".into()],
            pot_bb: 6.0,
            hero_oop: false,
            villain_action: "checks".into(),
            strategies: vec![HandStrategy {
                hand: "AsKs".into(),
                strategy: NodeStrategy {
                    actions: vec!["Check".into(), "Bet 2.0bb".into()],
                    frequencies: vec![0.25, 0.75],
                    action_ev: vec![1.0, 3.5],
                },
            }],
        }
    }

    #[test]
    fn best_picks_max_ev() {
        assert_eq!(sample_spot().strategies[0].strategy.best(), 1);
    }

    #[test]
    fn best_empty_is_zero() {
        let ns = NodeStrategy {
            actions: vec![],
            frequencies: vec![],
            action_ev: vec![],
        };
        assert_eq!(ns.best(), 0);
    }

    #[test]
    fn ev_loss_is_gap_to_best_clamped() {
        let ns = &sample_spot().strategies[0].strategy;
        assert_eq!(ns.ev_loss(1), 0.0); // best action: no loss
        assert!((ns.ev_loss(0) - 2.5).abs() < 1e-6); // 3.5 - 1.0
    }

    #[test]
    fn load_reads_json_and_skips_other_files() {
        let dir = std::env::temp_dir().join(format!("pt-load-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("spot.json"),
            serde_json::to_string(&sample_spot()).unwrap(),
        )
        .unwrap();
        fs::write(dir.join("README.txt"), "not json").unwrap();

        let provider = FileSolutionProvider::load(&dir).unwrap();
        assert_eq!(provider.spots().len(), 1);
        assert_eq!(provider.spots()[0].strategies[0].hand, "AsKs");

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn flop_key_is_order_independent() {
        // File stem keeps the user's order; the board field is solver-sorted.
        assert_eq!(flop_key("6h5d4c"), flop_key("4c5d6h"));
        assert_eq!(flop_key("Td9d6h"), flop_key(&["Td", "9d", "6h"].join("")));
        assert_ne!(flop_key("6h5d4c"), flop_key("6h5d4d"));
    }

    #[test]
    fn is_custom_only_when_a_field_overrides() {
        assert!(!SolveRequest::new("Td9d6h").is_custom());
        let mut r = SolveRequest::new("Td9d6h");
        r.sizes = Some("50%".into());
        assert!(r.is_custom());
    }

    #[test]
    fn solve_gen_args_forwards_only_set_fields() {
        let req = SolveRequest::new("Td9d6h");
        assert_eq!(
            solve_gen_args(&req, Path::new("data/solutions")),
            ["solve", "--flop", "Td9d6h", "--out", "data/solutions"]
        );

        let mut req = SolveRequest::new("Td9d6h");
        req.sizes = Some("50%, 100%".into());
        req.pot = Some(8.0);
        assert_eq!(
            solve_gen_args(&req, Path::new("/tmp/sol")),
            [
                "solve",
                "--flop",
                "Td9d6h",
                "--sizes",
                "50%, 100%",
                "--pot",
                "8",
                "--out",
                "/tmp/sol"
            ]
        );
    }
}
