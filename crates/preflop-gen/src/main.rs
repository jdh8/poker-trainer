//! Offline preflop chart generator (design doc 07).
//!
//! Permissive counterpart to `solve-gen`: original MCCFR over 169 hand
//! classes on the capped preflop tree defined in `game` — no AGPL solver
//! anywhere near this crate. Subcommands land milestone by milestone;
//! today: `tree` (tree statistics — the debug and regression tool) and
//! `equity` (the one-time exact heads-up table).

// Scaffold: game/equity/icm expose the full APIs; the solver milestones
// (design 07 M3+) consume the parts today's subcommands don't.
#![allow(dead_code)]

mod equity;
mod export;
mod game;
mod icm;
mod mccfr;

use clap::{Parser, Subcommand};
use std::path::{Path, PathBuf};

#[derive(Parser)]
#[command(version, about = "Preflop chart generator for poker-trainer")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Print tree statistics for a ruleset (no solving).
    Tree {
        /// Ruleset TOML, e.g. manifests/preflop/cash100.toml.
        #[arg(long)]
        ruleset: PathBuf,
    },
    /// Enumerate the exact 169×169 heads-up equity table (hours of CPU —
    /// run under scripts/idle-run.sh; needed once, then committed).
    Equity {
        /// Output path.
        #[arg(long, default_value = "data/preflop/equity-hu-169.json")]
        out: PathBuf,
        /// Worker threads (default: all-but-two cores).
        #[arg(long)]
        threads: Option<usize>,
    },
    /// Solve one ruleset and export its charts (minutes; wrap 6-max runs in
    /// scripts/idle-run.sh on the shared box).
    Solve {
        /// Ruleset TOML, e.g. manifests/preflop/cash100.toml.
        #[arg(long)]
        ruleset: PathBuf,
        /// Output directory (default: `data/preflop/<id>`).
        #[arg(long)]
        out: Option<PathBuf>,
        /// Value see-a-flop terminals as check-down equity (R ≡ 1) — the A/B
        /// baseline for the realization factors.
        #[arg(long)]
        check_down: bool,
        /// Override the manifest's traversal budget (quick smoke solves).
        #[arg(long)]
        traversals: Option<u64>,
    },
    /// Solve every ruleset in the manifests dir whose committed header hash
    /// is stale, then refresh data/preflop/index.json. Resumable: unchanged
    /// rulesets are skipped.
    Gen {
        /// Directory of ruleset TOMLs.
        #[arg(long, default_value = "manifests/preflop")]
        manifests: PathBuf,
        /// Output data directory.
        #[arg(long, default_value = "data/preflop")]
        out: PathBuf,
    },
    /// Refresh data/preflop/index.json from the solved ruleset dirs, without
    /// solving anything. Safe to run while other solves are in flight (e.g. to
    /// list rulesets built one-off via `solve`).
    Index {
        /// Output data directory.
        #[arg(long, default_value = "data/preflop")]
        out: PathBuf,
    },
}

/// The committed exact HU equity table (solves need it).
const HU_TABLE: &str = "data/preflop/equity-hu-169.json";

/// High-reach probe states for the convergence-drift signal: each seat's
/// unopened decision plus BB defending a BTN 2.5bb open.
fn probe(solver: &mccfr::Solver, rs: &game::Ruleset) -> Vec<f32> {
    let mut out = Vec::new();
    for path in ["", "f", "f-f", "f-f-f", "f-f-f-f", "f-f-f-r2.5-f"] {
        let Ok(st) = game::replay(rs, path) else {
            continue; // rulesets without a 2.5bb open just probe less
        };
        for class in 0..poker_trainer::preflop::CLASSES {
            if let Some(avg) = solver.average_at(&st, class) {
                out.extend(avg);
            }
        }
    }
    out
}

/// Solve one ruleset TOML into `out_dir` and export charts + header.
fn solve_one(
    toml_path: &Path,
    out_dir: &Path,
    check_down: bool,
    traversals_override: Option<u64>,
) -> Result<(), String> {
    let rs = game::Ruleset::load(toml_path)?;
    let config = export::config_echo(toml_path).map_err(|e| e.to_string())?;
    let cache = equity::EquityCache::load(HU_TABLE).map_err(|e| e.to_string())?;
    let mut solver = mccfr::Solver::new(&rs, cache);
    if check_down {
        solver = solver.check_down();
    }

    let total = traversals_override.unwrap_or(rs.solver.traversals);
    solver.set_avg_warmup(total / 5); // averages skip the noisy opening act
    let chunks = 10u64;
    let mut last_probe: Option<Vec<f32>> = None;
    let mut drift: Option<f32> = None;
    let started = std::time::Instant::now();
    for done in 1..=chunks {
        solver.run(total / chunks + u64::from(done == chunks) * (total % chunks));
        let p = probe(&solver, &rs);
        drift = last_probe.as_ref().filter(|q| q.len() == p.len()).map(|q| {
            p.iter()
                .zip(q.iter())
                .map(|(a, b)| (a - b).abs())
                .fold(0.0, f32::max)
        });
        last_probe = Some(p);
        eprintln!(
            "{}: {}/{total} hands, {:.0?} elapsed, {} states, drift {}",
            rs.id,
            solver.hands_dealt(),
            started.elapsed(),
            solver.infosets.len(),
            drift.map_or("n/a".into(), |d| format!("{d:.4}")),
        );
    }

    export::write_ruleset(&solver, &rs, config, drift, out_dir).map_err(|e| e.to_string())
}

fn main() -> Result<(), String> {
    match Cli::parse().cmd {
        Cmd::Tree { ruleset } => {
            let rs = game::Ruleset::load(&ruleset)?;
            let s = game::tree_stats(&rs);
            println!("{} ({} seats, {}bb):", rs.id, rs.n(), rs.stack_bb);
            println!("  decision histories  {:>12}", s.decisions);
            println!("  distinct states     {:>12}", s.states);
            println!("  action edges        {:>12}", s.edges);
            println!("  fold-win terminals  {:>12}", s.fold_wins);
            println!(
                "  all-in showdowns    {:>12}  ({} multiway)",
                s.allin_2way + s.allin_multi,
                s.allin_multi
            );
            println!(
                "  flops seen          {:>12}  ({} multiway)",
                s.flop_2way + s.flop_multi,
                s.flop_multi
            );
            println!("  max depth           {:>12}", s.max_depth);
            Ok(())
        }
        Cmd::Equity { out, threads } => {
            let threads = threads.unwrap_or_else(|| {
                std::thread::available_parallelism().map_or(1, |n| n.get().saturating_sub(2).max(1))
            });
            eprintln!("enumerating 14365 class pairs on {threads} threads…");
            let table = equity::gen_hu_table(threads);
            equity::save_hu_table(&out, &table).map_err(|e| e.to_string())?;
            println!("wrote {}", out.display());
            Ok(())
        }
        Cmd::Solve {
            ruleset,
            out,
            check_down,
            traversals,
        } => {
            let out = out.unwrap_or_else(|| {
                let rs = game::Ruleset::load(&ruleset);
                PathBuf::from("data/preflop").join(rs.map(|r| r.id).unwrap_or_default())
            });
            solve_one(&ruleset, &out, check_down, traversals)
        }
        Cmd::Gen { manifests, out } => {
            let mut tomls: Vec<PathBuf> = std::fs::read_dir(&manifests)
                .map_err(|e| format!("{}: {e}", manifests.display()))?
                .filter_map(|e| e.ok())
                .map(|e| e.path())
                .filter(|p| p.extension().is_some_and(|x| x == "toml"))
                .collect();
            tomls.sort();
            for toml_path in &tomls {
                let rs = game::Ruleset::load(toml_path)?;
                let echo = export::config_echo(toml_path).map_err(|e| e.to_string())?;
                let dir = out.join(&rs.id);
                let fresh = std::fs::read_to_string(dir.join("header.json"))
                    .ok()
                    .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
                    .is_some_and(|h| h["config_hash"] == export::config_hash(&echo));
                if fresh {
                    eprintln!("{}: up to date, skipping", rs.id);
                    continue;
                }
                solve_one(toml_path, &dir, false, None)?;
            }
            export::write_index(&out).map_err(|e| e.to_string())?;
            println!("index refreshed: {}", out.join("index.json").display());
            Ok(())
        }
        Cmd::Index { out } => {
            export::write_index(&out).map_err(|e| e.to_string())?;
            println!("index refreshed: {}", out.join("index.json").display());
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    /// The license posture, made executable (mirrors
    /// `trainer_never_links_the_solver` in src/solution.rs): this crate is
    /// MIT/Apache and must never link the AGPL postflop solver. If this
    /// fails, remove the dependency you added — do not touch the test.
    #[test]
    fn preflop_gen_never_links_the_solver() {
        let out = std::process::Command::new(env!("CARGO"))
            .args(["tree", "-p", "preflop-gen", "-e", "normal,build"])
            .output()
            .expect("run cargo tree");
        assert!(
            out.status.success(),
            "cargo tree failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let tree = String::from_utf8_lossy(&out.stdout);
        assert!(
            !tree.contains("postflop-solver"),
            "preflop-gen's dependency tree links the AGPL solver:\n{tree}"
        );
    }
}
