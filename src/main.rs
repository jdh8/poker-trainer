// Scaffold: lots of intentionally-unused stubs while modules are filled in.
#![allow(dead_code)]

mod board;
mod eval;
mod range;
mod solution;
mod trainer;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "poker-trainer", about = "Post-flop-focused GTO poker trainer")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run a training drill (present a spot, score your action vs. GTO).
    Drill,
    /// Solve a custom spot and cache it (phase 3 — not implemented yet).
    Solve,
}

fn main() {
    match Cli::parse().command {
        Command::Drill => trainer::run_drill(),
        Command::Solve => eprintln!("solve: not implemented yet (phase 3)"),
    }
}
