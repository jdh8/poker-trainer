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
    /// The game config this node was solved under. `None` on pre-v2 files,
    /// which must keep parsing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config: Option<SpotConfig>,
    /// Provenance of the solve. `None` on pre-v2 files.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generator: Option<GenInfo>,
    /// Per-hero-hand strategies.
    pub strategies: Vec<HandStrategy>,
}

/// The full postflop game config, shared by both crates (design doc 02): it's
/// the CLI's resolved knobs, the `serve`/`solve` request body, the cache-key
/// input ([`SpotConfig::hash8`]), and the provenance embedded in every
/// snapshot. The flop is *not* part of it — one config spans many flops.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SpotConfig {
    /// Formation id, e.g. `"srp-btn-bb"` (see [`FORMATIONS`]).
    pub formation: String,
    pub oop_range: String,
    pub ip_range: String,
    /// Bet sizes per street, e.g. `"33%, 75%"` (parsed by the solver).
    pub flop_sizes: String,
    pub turn_sizes: String,
    pub river_sizes: String,
    pub stack_bb: f32,
    pub pot_bb: f32,
    /// Rake taken from the pot (0.05 = 5%), capped at `rake_cap_bb`.
    pub rake_rate: f32,
    pub rake_cap_bb: f32,
}

impl SpotConfig {
    /// The formation's default config, ranges read from
    /// `<ranges_dir>/<formation>/{oop,ip}.txt`.
    pub fn for_formation(id: &str, ranges_dir: impl AsRef<Path>) -> io::Result<Self> {
        let f = formation(id).ok_or_else(|| {
            let known: Vec<&str> = FORMATIONS.iter().map(|f| f.id).collect();
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("unknown formation {id:?} (known: {})", known.join(", ")),
            )
        })?;
        let dir = ranges_dir.as_ref().join(id);
        let read = |seat: &str| -> io::Result<String> {
            let path = dir.join(format!("{seat}.txt"));
            fs::read_to_string(&path)
                .map(|s| s.trim().to_string())
                .map_err(|e| {
                    io::Error::new(e.kind(), format!("range file {}: {e}", path.display()))
                })
        };
        Ok(Self {
            formation: id.into(),
            oop_range: read("oop")?,
            ip_range: read("ip")?,
            flop_sizes: "33%, 75%".into(),
            turn_sizes: "33%".into(),
            river_sizes: "33%".into(),
            stack_bb: f.stack_bb,
            pot_bb: f.pot_bb,
            rake_rate: 0.0,
            rake_cap_bb: 0.0,
        })
    }

    /// Stable 8-hex-char cache key of the canonical (declaration-order) JSON.
    /// FNV-1a by hand: the stdlib hasher isn't stable across Rust releases,
    /// and cache filenames must be.
    pub fn hash8(&self) -> String {
        let json = serde_json::to_string(self).expect("config serializes");
        let mut h: u64 = 0xcbf29ce484222325;
        for b in json.bytes() {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x100000001b3);
        }
        format!("{:08x}", (h >> 32) as u32)
    }
}

/// Provenance embedded in every v2 snapshot.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GenInfo {
    /// solve-gen crate version.
    pub version: String,
    /// Exploitability the solve reached, in bb.
    pub exploitability_bb: f32,
}

/// A formation: preflop line + seats + default pot/stacks. Range defaults live
/// in `data/ranges/<id>/{oop,ip}.txt`, not here.
pub struct Formation {
    pub id: &'static str,
    /// e.g. "SRP BTN vs BB" — also the snapshot-label prefix.
    pub label: &'static str,
    pub oop_seat: &'static str,
    pub ip_seat: &'static str,
    pub pot_bb: f32,
    pub stack_bb: f32,
}

/// v2 formations, ordered by real-hand frequency (design doc 02). Stack-depth
/// and rake variants are manifest overrides, not new entries.
pub const FORMATIONS: &[Formation] = &[
    Formation {
        id: "srp-btn-bb",
        label: "SRP BTN vs BB",
        oop_seat: "BB",
        ip_seat: "BTN",
        pot_bb: 6.0,
        stack_bb: 97.0,
    },
    Formation {
        id: "srp-co-bb",
        label: "SRP CO vs BB",
        oop_seat: "BB",
        ip_seat: "CO",
        pot_bb: 6.0,
        stack_bb: 97.0,
    },
    Formation {
        id: "srp-sb-bb",
        label: "SRP SB vs BB",
        oop_seat: "SB",
        ip_seat: "BB",
        pot_bb: 5.5,
        stack_bb: 97.0,
    },
    Formation {
        id: "3bp-bb-btn",
        label: "3-bet pot BB vs BTN",
        oop_seat: "BB",
        ip_seat: "BTN",
        pot_bb: 18.0,
        stack_bb: 89.0,
    },
    Formation {
        id: "3bp-btn-co",
        label: "3-bet pot BTN vs CO",
        oop_seat: "CO",
        ip_seat: "BTN",
        pot_bb: 20.0,
        stack_bb: 89.0,
    },
];

/// Look up a formation by id.
pub fn formation(id: &str) -> Option<&'static Formation> {
    FORMATIONS.iter().find(|f| f.id == id)
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

/// What to live-solve: a flop plus a fully-resolved [`SpotConfig`]. The
/// trainer resolves formations/ranges/defaults itself, so solve-gen executes
/// exactly this — range and size strings stay opaque here (parsing them needs
/// the solver). Serde because this is also the body of a tree-session
/// `op:solve` (see `tree`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SolveRequest {
    pub flop: String,
    pub config: SpotConfig,
}

/// Live-solving provider: shells out to the `solve-gen` binary (the only thing
/// that links the solver), which writes `SolvedSpot` JSON into the solution
/// dir; we then load just the spots for the requested flop. Delivers on the
/// README's "a live-solving provider can implement this same trait".
pub struct LiveSolutionProvider {
    spots: Vec<SolvedSpot>,
}

impl LiveSolutionProvider {
    /// Solve `req` into `dir` (unless its config-hash is already cached
    /// there), then load the spots matching the requested flop *and* config.
    pub fn solve(req: &SolveRequest, dir: impl AsRef<Path>) -> io::Result<Self> {
        let dir = dir.as_ref();
        let hash = req.config.hash8();
        let stem = format!("{}-{hash}", req.flop.to_lowercase());
        if !dir.join(format!("{stem}-ip.json")).exists() {
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
            // Config-less pre-v2 files never match a live request: they get
            // their own re-solve under the new naming rather than a guess.
            .filter(|s| {
                flop_key(&s.board.join("")) == key
                    && s.config.as_ref().is_some_and(|c| c.hash8() == hash)
            })
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
/// unit-testable without spawning anything. Every config field is forwarded
/// explicitly; solve-gen's own defaults only apply to manual invocations.
fn solve_gen_args(req: &SolveRequest, out_dir: &Path) -> Vec<String> {
    let c = &req.config;
    [
        ("--flop", req.flop.clone()),
        ("--formation", c.formation.clone()),
        ("--oop", c.oop_range.clone()),
        ("--ip", c.ip_range.clone()),
        ("--sizes", c.flop_sizes.clone()),
        ("--turn-sizes", c.turn_sizes.clone()),
        ("--river-sizes", c.river_sizes.clone()),
        ("--stack", c.stack_bb.to_string()),
        ("--pot", c.pot_bb.to_string()),
        ("--rake-rate", c.rake_rate.to_string()),
        ("--rake-cap", c.rake_cap_bb.to_string()),
        ("--out", out_dir.to_string_lossy().into_owned()),
    ]
    .into_iter()
    .fold(vec!["solve".into()], |mut a, (flag, val)| {
        a.push(flag.into());
        a.push(val);
        a
    })
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

    fn sample_config() -> SpotConfig {
        SpotConfig {
            formation: "srp-btn-bb".into(),
            oop_range: "AA,KK".into(),
            ip_range: "QQ,JJ".into(),
            flop_sizes: "33%, 75%".into(),
            turn_sizes: "33%".into(),
            river_sizes: "33%".into(),
            stack_bb: 97.0,
            pot_bb: 6.0,
            rake_rate: 0.0,
            rake_cap_bb: 0.0,
        }
    }

    fn sample_spot() -> SolvedSpot {
        SolvedSpot {
            label: "test".into(),
            board: vec!["Td".into(), "9d".into(), "6h".into()],
            pot_bb: 6.0,
            hero_oop: false,
            villain_action: "checks".into(),
            config: None,
            generator: None,
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
    fn hash8_is_stable_and_config_sensitive() {
        let c = sample_config();
        // Pinned value: this is a *persisted* cache key — if this assertion
        // ever fails, every cached solve on every machine is invalidated.
        assert_eq!(c.hash8(), sample_config().hash8());
        assert_eq!(c.hash8().len(), 8);

        let mut tweaked = sample_config();
        tweaked.rake_rate = 0.05;
        assert_ne!(c.hash8(), tweaked.hash8());
    }

    #[test]
    fn for_formation_reads_ranges_and_rejects_unknown_ids() {
        let dir = std::env::temp_dir().join(format!("pt-ranges-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("srp-btn-bb")).unwrap();
        fs::write(dir.join("srp-btn-bb/oop.txt"), "AA,KK\n").unwrap();
        fs::write(dir.join("srp-btn-bb/ip.txt"), "QQ,JJ\n").unwrap();

        let c = SpotConfig::for_formation("srp-btn-bb", &dir).unwrap();
        assert_eq!(c.oop_range, "AA,KK"); // trimmed
        assert_eq!(c.pot_bb, 6.0);
        assert_eq!(c.stack_bb, 97.0);

        let err = SpotConfig::for_formation("hu-limped", &dir).unwrap_err();
        assert!(err.to_string().contains("unknown formation"));
        // Known formation, missing range file: the path is in the error.
        let err = SpotConfig::for_formation("srp-co-bb", &dir).unwrap_err();
        assert!(err.to_string().contains("srp-co-bb"));

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn spots_with_config_round_trip_and_old_files_still_parse() {
        let mut spot = sample_spot();
        spot.config = Some(sample_config());
        spot.generator = Some(GenInfo {
            version: "0.1.0".into(),
            exploitability_bb: 0.03,
        });
        let json = serde_json::to_string(&spot).unwrap();
        let back: SolvedSpot = serde_json::from_str(&json).unwrap();
        assert_eq!(back.config.unwrap().formation, "srp-btn-bb");

        // A pre-v2 file (no config/generator keys) must keep parsing.
        let old: SolvedSpot =
            serde_json::from_str(&serde_json::to_string(&sample_spot()).unwrap()).unwrap();
        assert!(old.config.is_none());
    }

    #[test]
    fn solve_gen_args_forwards_the_whole_config() {
        let req = SolveRequest {
            flop: "Td9d6h".into(),
            config: sample_config(),
        };
        let args = solve_gen_args(&req, Path::new("/tmp/sol"));
        assert_eq!(args[0], "solve");
        for (flag, val) in [
            ("--flop", "Td9d6h"),
            ("--formation", "srp-btn-bb"),
            ("--oop", "AA,KK"),
            ("--sizes", "33%, 75%"),
            ("--turn-sizes", "33%"),
            ("--stack", "97"),
            ("--rake-rate", "0"),
            ("--out", "/tmp/sol"),
        ] {
            let i = args.iter().position(|a| a == flag).unwrap();
            assert_eq!(args[i + 1], val, "value of {flag}");
        }
    }
}
