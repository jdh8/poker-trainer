//! Offline preflop chart generator (design doc 07).
//!
//! Permissive counterpart to `solve-gen`: original MCCFR over 169 hand
//! classes on the capped preflop tree defined in `game` — no AGPL solver
//! anywhere near this crate. Subcommands land milestone by milestone;
//! today: `tree` (tree statistics — the debug and regression tool).

// Scaffold: game exposes the full state-machine API; the solver milestones
// (design 07 M2+) consume the parts the `tree` subcommand doesn't.
#![allow(dead_code)]

mod game;

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
    }
}
