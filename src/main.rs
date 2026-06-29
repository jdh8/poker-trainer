use clap::{Parser, Subcommand, ValueEnum};
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
    },
}

#[derive(Clone, Copy, ValueEnum)]
enum Mode {
    /// Call/fold vs. break-even pot odds (Monte-Carlo equity).
    PotOdds,
    /// Classify the flop's board texture.
    Texture,
    /// Act vs. a precomputed GTO solution; scored on EV loss.
    Gto,
}

fn main() {
    match Cli::parse().command {
        Command::Drill { mode } => match mode {
            Mode::PotOdds => trainer::run_pot_odds_drill(),
            Mode::Texture => trainer::run_texture_drill(),
            Mode::Gto => trainer::run_gto_drill(),
        },
    }
}
