use clap::{Parser, Subcommand, ValueEnum};
use poker_trainer::solution::SolveRequest;
use poker_trainer::trainer;

#[derive(Parser)]
#[command(name = "poker-trainer", about = "Post-flop-focused GTO poker trainer")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run a training drill (present a spot, score your action).
    Drill {
        /// Which drill to run.
        #[arg(value_enum, default_value_t = Mode::PotOdds)]
        mode: Mode,

        /// Live-solve this flop (e.g. Td9d6h) for the gto/range drills instead
        /// of a curated spot. Expect ~30 s, ~1 GB RAM. Cached in data/solutions.
        /// Ignored by pot-odds/texture.
        #[arg(long)]
        board: Option<String>,
        /// OOP (BB) range for --board (defaults to solve-gen's wide range).
        #[arg(long)]
        oop: Option<String>,
        /// IP (BTN) range for --board.
        #[arg(long)]
        ip: Option<String>,
        /// Flop c-bet sizes for --board, e.g. "33%, 75%".
        #[arg(long)]
        sizes: Option<String>,
        /// Effective stack in bb for --board.
        #[arg(long)]
        stack: Option<f32>,
        /// Starting pot in bb for --board.
        #[arg(long)]
        pot: Option<f32>,
    },
}

#[derive(Clone, Copy, ValueEnum)]
enum Mode {
    /// Call/fold vs. break-even pot odds (Monte-Carlo equity).
    PotOdds,
    /// Classify the flop's board texture.
    Texture,
    /// Act vs. a GTO solution; scored on EV loss. `--board` live-solves a spot.
    Gto,
    /// Assign an action for your whole range; scored with per-bucket leak stats.
    Range,
}

fn main() {
    match Cli::parse().command {
        Command::Drill {
            mode,
            board,
            oop,
            ip,
            sizes,
            stack,
            pot,
        } => {
            let req = board.map(|flop| SolveRequest {
                flop,
                oop,
                ip,
                sizes,
                stack,
                pot,
            });
            match mode {
                Mode::PotOdds => trainer::run_pot_odds_drill(),
                Mode::Texture => trainer::run_texture_drill(),
                Mode::Gto => trainer::run_gto_drill(req),
                Mode::Range => trainer::run_range_drill(req),
            }
        }
    }
}
