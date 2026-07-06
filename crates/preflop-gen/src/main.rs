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
mod game;
mod icm;
mod mccfr;

use clap::{Parser, Subcommand};
use std::path::PathBuf;

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
        /// Time one class pair and exit (sizing the full run).
        #[arg(long, hide = true)]
        bench_pair: bool,
    },
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
        Cmd::Equity {
            out,
            threads,
            bench_pair,
        } => {
            if bench_pair {
                let t = std::time::Instant::now();
                let e = equity::exact_pair_equity(0, 14); // AA vs KK
                println!(
                    "AA vs KK = {e:.8} in {:.2?} (×14365 pairs / threads)",
                    t.elapsed()
                );
                return Ok(());
            }
            let threads = threads.unwrap_or_else(|| {
                std::thread::available_parallelism().map_or(1, |n| n.get().saturating_sub(2).max(1))
            });
            eprintln!("enumerating 14365 class pairs on {threads} threads…");
            let table = equity::gen_hu_table(threads);
            equity::save_hu_table(&out, &table).map_err(|e| e.to_string())?;
            println!("wrote {}", out.display());
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
