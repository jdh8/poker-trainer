use clap::{Args, Parser, Subcommand, ValueEnum};
use poker_trainer::solution::{SolveRequest, SpotConfig};
use poker_trainer::{analyze, report, stats, trainer};
use std::path::PathBuf;

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

        /// Preflop drill only: the solved chart set under data/preflop/
        /// (e.g. cash100, cash20).
        #[arg(long, default_value = "cash100")]
        ruleset: String,

        #[command(flatten)]
        solve: SolveArgs,
    },
    /// Browse a solved spot's full strategy as a 13×13 grid (GTO-Wizard style).
    Table {
        #[command(flatten)]
        solve: SolveArgs,
        /// With --board: descend this action line before the browser opens,
        /// e.g. "Check,Bet 2.0bb,deal 2c" (as printed by analyze's blunders).
        #[arg(long)]
        line: Option<String>,
        /// Lock file (written by `S` in the lock editor): if it exists, its
        /// line + cell locks are replayed on startup; either way it's where
        /// `S` saves. Needs --board.
        #[arg(long)]
        locks: Option<PathBuf>,
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
    /// Aggregate flop report over the snapshot library (one row per flop/node).
    Report {
        /// Only include spots from this formation (e.g. srp-btn-bb).
        #[arg(long)]
        formation: Option<String>,
        /// Only include IP or OOP nodes.
        #[arg(long, value_enum)]
        node: Option<report::NodeSide>,
        /// Row sort order.
        #[arg(long, value_enum, default_value = "texture")]
        sort: report::Sort,
        /// Write rows as CSV to this file instead of a terminal table.
        #[arg(long)]
        csv: Option<PathBuf>,
    },
    /// Import PokerStars/GGPoker hand histories, score your decisions against
    /// the library equilibrium, and report EV-loss leaks + top blunders.
    Analyze {
        /// Hand-history text files.
        #[arg(required = true)]
        files: Vec<PathBuf>,
        /// Parse + match only — coverage stats without touching a solver.
        #[arg(long)]
        dry_run: bool,
        /// Wall-clock budget for solving spots, e.g. "10m", "45s"; spots
        /// beyond it (least-frequent first) are reported as unscored.
        #[arg(long, default_value = "10m")]
        solve_budget: String,
        /// Also write every scored decision as JSONL to this file.
        #[arg(long)]
        jsonl: Option<PathBuf>,
    },
    /// Range-vs-range equity on a flop, with a per-range equity histogram.
    Equity {
        /// OOP range, e.g. "22+,A2s+,KTo+".
        #[arg(long)]
        oop: String,
        /// IP range.
        #[arg(long)]
        ip: String,
        /// Flop, e.g. Td9d6h.
        #[arg(long)]
        board: String,
    },
}

/// The live-solve knobs shared by `drill gto`/`drill range`/`drill hand`/
/// `table`. With `--board` set they live-solve that flop under `--formation`'s
/// config (ranges from data/ranges/, overridable per flag); without it, the
/// curated library is used. Ignored by `drill pot-odds`.
#[derive(Args)]
struct SolveArgs {
    /// Live-solve this flop (e.g. Td9d6h) instead of a curated spot. Expect
    /// ~30 s, ~1 GB RAM. Cached in data/solutions, keyed by config hash.
    #[arg(long)]
    board: Option<String>,
    /// Formation for --board: seats, default pot/stacks, and the ranges read
    /// from data/ranges/<formation>/{oop,ip}.txt.
    #[arg(long, default_value = "srp-btn-bb")]
    formation: String,
    /// OOP range override for --board.
    #[arg(long)]
    oop: Option<String>,
    /// IP range override for --board.
    #[arg(long)]
    ip: Option<String>,
    /// Flop bet sizes for --board, e.g. "33%, 75%".
    #[arg(long)]
    sizes: Option<String>,
    /// Turn bet sizes for --board.
    #[arg(long)]
    turn_sizes: Option<String>,
    /// River bet sizes for --board.
    #[arg(long)]
    river_sizes: Option<String>,
    /// Effective stack in bb for --board.
    #[arg(long)]
    stack: Option<f32>,
    /// Starting pot in bb for --board.
    #[arg(long)]
    pot: Option<f32>,
    /// Rake rate for --board (0.05 = 5%).
    #[arg(long)]
    rake_rate: Option<f32>,
    /// Rake cap in bb for --board.
    #[arg(long)]
    rake_cap: Option<f32>,
}

impl SolveArgs {
    /// `Some(request)` when `--board` is given, else `None` (use the library).
    /// A bad formation or missing range file prints and exits — there's
    /// nothing to drill without a config.
    fn into_request(self) -> Option<SolveRequest> {
        let flop = self.board?;
        let mut config =
            SpotConfig::for_formation(&self.formation, "data/ranges").unwrap_or_else(|e| {
                eprintln!("{e}");
                std::process::exit(2);
            });
        let overrides = [
            (&mut config.oop_range, self.oop),
            (&mut config.ip_range, self.ip),
            (&mut config.flop_sizes, self.sizes),
            (&mut config.turn_sizes, self.turn_sizes),
            (&mut config.river_sizes, self.river_sizes),
        ];
        for (field, value) in overrides {
            if let Some(v) = value {
                *field = v;
            }
        }
        if let Some(v) = self.stack {
            config.stack_bb = v;
        }
        if let Some(v) = self.pot {
            config.pot_bb = v;
        }
        if let Some(v) = self.rake_rate {
            config.rake_rate = v;
        }
        if let Some(v) = self.rake_cap {
            config.rake_cap_bb = v;
        }
        Some(SolveRequest { flop, config })
    }
}

#[derive(Clone, Copy, ValueEnum)]
enum Mode {
    /// Call/fold vs. break-even pot odds (Monte-Carlo equity).
    PotOdds,
    /// Act vs. a GTO solution; scored on EV loss. `--board` live-solves a spot.
    Gto,
    /// Assign an action for your whole range; scored with per-bucket leak stats.
    Range,
    /// Play full hands (flop→river) vs. the equilibrium villain; needs --board.
    Hand,
    /// Preflop decisions vs. the solved 6-max charts (data/preflop/), scored
    /// on EV loss; pick the rule set with --ruleset.
    Preflop,
}

fn main() {
    match Cli::parse().command {
        Command::Drill {
            mode,
            solve,
            ruleset,
        } => {
            let req = solve.into_request();
            match mode {
                Mode::PotOdds => trainer::run_pot_odds_drill(),
                Mode::Gto => trainer::run_gto_drill(req),
                Mode::Range => trainer::run_range_drill(req),
                Mode::Hand => trainer::run_hand_drill(req),
                Mode::Preflop => trainer::run_preflop_drill(&ruleset),
            }
        }
        Command::Table { solve, line, locks } => {
            trainer::run_table(solve.into_request(), line, locks)
        }
        Command::Stats { by, last } => stats::run(by, last),
        Command::Report {
            formation,
            node,
            sort,
            csv,
        } => report::run_report(formation, node, sort, csv.as_deref()),
        Command::Analyze {
            files,
            dry_run,
            solve_budget,
            jsonl,
        } => analyze::run(&files, dry_run, &solve_budget, jsonl.as_deref()),
        Command::Equity { oop, ip, board } => report::run_equity(&oop, &ip, &board),
    }
}
