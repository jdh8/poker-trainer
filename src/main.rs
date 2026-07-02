use clap::{Args, Parser, Subcommand, ValueEnum};
use poker_trainer::solution::SolveRequest;
use poker_trainer::{stats, trainer};

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

        #[command(flatten)]
        solve: SolveArgs,
    },
    /// Browse a solved spot's full strategy as a 13×13 grid (GTO-Wizard style).
    Table {
        #[command(flatten)]
        solve: SolveArgs,
    },
    /// Report your recorded drill history as a leak profile.
    Stats {
        /// Group decisions by this dimension.
        #[arg(long, value_enum, default_value = "bucket")]
        by: stats::GroupBy,
        /// Only consider the last N decisions.
        #[arg(long)]
        last: Option<usize>,
    },
}

/// The live-solve knobs shared by `drill gto`/`drill range`/`table`. With
/// `--board` set they live-solve that flop (and `--oop/--ip/…` forward straight
/// to solve-gen); without it, the curated library is used. Ignored by
/// `drill pot-odds`/`drill texture`.
#[derive(Args)]
struct SolveArgs {
    /// Live-solve this flop (e.g. Td9d6h) instead of a curated spot. Expect
    /// ~30 s, ~1 GB RAM. Cached in data/solutions.
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
}

impl SolveArgs {
    /// `Some(request)` when `--board` is given, else `None` (use the library).
    fn into_request(self) -> Option<SolveRequest> {
        self.board.map(|flop| SolveRequest {
            flop,
            oop: self.oop,
            ip: self.ip,
            sizes: self.sizes,
            stack: self.stack,
            pot: self.pot,
        })
    }
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
    /// Play full hands (flop→river) vs. the equilibrium villain; needs --board.
    Hand,
}

fn main() {
    match Cli::parse().command {
        Command::Drill { mode, solve } => {
            let req = solve.into_request();
            match mode {
                Mode::PotOdds => trainer::run_pot_odds_drill(),
                Mode::Texture => trainer::run_texture_drill(),
                Mode::Gto => trainer::run_gto_drill(req),
                Mode::Range => trainer::run_range_drill(req),
                Mode::Hand => trainer::run_hand_drill(req),
            }
        }
        Command::Table { solve } => trainer::run_table(solve.into_request()),
        Command::Stats { by, last } => stats::run(by, last),
    }
}
