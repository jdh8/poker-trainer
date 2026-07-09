use clap::{Args, Parser, Subcommand, ValueEnum};
use poker_trainer::preflop::{
    class_index, class_name, parse_cards, weighted_range_string, PreflopCharts,
};
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
        /// (e.g. cash89, cash21).
        #[arg(long, default_value = "cash89")]
        ruleset: String,

        /// Pot-odds drill only: draw flop spots (and villain's range) from this
        /// heads-up preflop chart set, e.g. cash-hu89, mtt-hu21. Defaults to
        /// cash-hu89.
        #[arg(long)]
        preflop: Option<String>,

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
    /// Ground a postflop solve in a preflop line: print the `solve-gen solve`
    /// arguments (weighted arrival ranges + pot + effective stack + rake) for a
    /// flop-closing line of a solved chart set. Omit --line to list the lines.
    /// Name your seat with --hero to see the spot as hero vs. villain (and which
    /// side, OOP/IP, is yours); add --hand to check how often you get there.
    ExportRange {
        /// Solved preflop chart set under data/preflop/ (e.g. cash-hu55,
        /// cash89). Any line where exactly two seats reach the flop works.
        #[arg(long)]
        ruleset: String,
        /// Flop-closing action line, e.g. "r2.5-c" (SB opens 2.5bb, BB calls).
        /// Omit to list the ruleset's flop-closing lines.
        #[arg(long)]
        line: Option<String>,
        /// A flop (e.g. Td9d6h): prints a ready-to-run solve-gen command instead
        /// of just the range/pot/stack arguments.
        #[arg(long)]
        flop: Option<String>,
        /// Hero's seat, e.g. SB or BTN — must be one of the two seats that reach
        /// the flop. Reframes the output as hero/villain and reports which side
        /// (OOP/IP) is yours. The emitted solve args are unchanged.
        #[arg(long)]
        hero: Option<String>,
        /// Hero's hole cards, e.g. AhKh (needs --hero) — reports how often hero
        /// reaches this line with that hand. Provenance only: the solve is over
        /// hero's whole range.
        #[arg(long)]
        hand: Option<String>,
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
    /// Ground --board in a solved preflop line: `<ruleset>:<line>`, e.g.
    /// cash-hu55:r2.5-c. Ranges/pot/stack/rake come from that equilibrium
    /// (as `export-range` would emit) instead of --formation's authored guess;
    /// the remaining overrides still apply on top. List lines with `export-range
    /// --ruleset <id>`.
    #[arg(long)]
    from: Option<String>,
}

impl SolveArgs {
    /// `Some(request)` when `--board` is given, else `None` (use the library).
    /// A bad formation/spec or missing range file prints and exits — there's
    /// nothing to drill without a config.
    fn into_request(self) -> Option<SolveRequest> {
        let flop = self.board?;
        let mut config = match &self.from {
            Some(spec) => grounded_config(spec),
            None => SpotConfig::for_formation(&self.formation, "data/ranges").unwrap_or_else(|e| {
                eprintln!("{e}");
                std::process::exit(2);
            }),
        };
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
            preflop,
        } => {
            let req = solve.into_request();
            match mode {
                Mode::PotOdds => trainer::run_pot_odds_drill(preflop.as_deref()),
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
        Command::ExportRange {
            ruleset,
            line,
            flop,
            hero,
            hand,
        } => run_export_range(
            &ruleset,
            line.as_deref(),
            flop.as_deref(),
            hero.as_deref(),
            hand.as_deref(),
        ),
    }
}

/// Position-derived solve inputs for a flop-closing preflop `line`: both live
/// seats (OOP, IP), their per-class arrival reaches + weighted range strings,
/// the pot, effective stack, and rake — everything `solve-gen solve` needs,
/// plus the reaches for hero/hand provenance.
struct LineSpot {
    oop_seat: String,
    ip_seat: String,
    oop_reach: Vec<f32>,
    ip_reach: Vec<f32>,
    oop_range: String,
    ip_range: String,
    pot: f32,
    stack: f32,
    rake_rate: f32,
    rake_cap: f32,
}

/// Condense a solved preflop `line` into its two-player flop [`LineSpot`].
/// `Err` (user-facing) if the line isn't a clean two-player flop close or an
/// ancestor node was pruned — everything the caller should report and bail on.
fn derive_line_spot(charts: &PreflopCharts, line: &str) -> Result<LineSpot, String> {
    let (oop_seat, ip_seat) = charts.flop_seats(line).ok_or_else(|| {
        format!(
            "{line:?} isn't a two-player flop line (list lines with: export-range --ruleset <id>)"
        )
    })?;
    let pot = charts.flop_pot_bb(line).ok_or_else(|| {
        format!("{line:?} doesn't close to a flop (more action follows, or it's a fold/all-in)")
    })?;
    let reach = |seat: &str| {
        charts
            .seat_reach(line, seat)
            .ok_or_else(|| format!("{line:?} has a pruned/missing ancestor node"))
    };
    let oop_reach = reach(&oop_seat)?;
    let ip_reach = reach(&ip_seat)?;
    let oop_range = weighted_range_string(&oop_reach);
    let ip_range = weighted_range_string(&ip_reach);
    if oop_range.is_empty() || ip_range.is_empty() {
        return Err(format!(
            "{line:?} is unreachable for a live seat under the equilibrium"
        ));
    }
    let stack = charts
        .stack_bb()
        .ok_or_else(|| "ruleset config lacks stack_bb".to_string())?
        - charts.line_commitment_bb(line);
    let (rake_rate, rake_cap) = charts.rake();
    Ok(LineSpot {
        oop_seat,
        ip_seat,
        oop_reach,
        ip_reach,
        oop_range,
        ip_range,
        pot,
        stack,
        rake_rate,
        rake_cap,
    })
}

/// Base [`SpotConfig`] for a `<ruleset>:<line>` spec (the `--from` flag): the
/// `srp-btn-bb` shell — solver-default sizes and a formation solve-gen's
/// `for_formation` accepts, so the cache key matches the `export-range`-emitted
/// command — with ranges/pot/stack/rake replaced by the preflop equilibrium.
/// Prints and exits on a bad spec (there's nothing to solve without it).
fn grounded_config(spec: &str) -> SpotConfig {
    let die = |msg: String| -> ! {
        eprintln!("{msg}");
        std::process::exit(2);
    };
    let (ruleset, line) = spec.split_once(':').unwrap_or_else(|| {
        die(format!(
            "--from expects <ruleset>:<line>, e.g. cash-hu55:r2.5-c (got {spec:?})"
        ))
    });
    let charts = PreflopCharts::load(format!("data/preflop/{ruleset}"))
        .unwrap_or_else(|e| die(e.to_string()));
    let s = derive_line_spot(&charts, line).unwrap_or_else(|e| die(e));
    eprintln!(
        "# grounded in {} line {line} — OOP={} IP={} pot={:.2}bb eff_stack={:.2}bb rake={}/{}bb",
        charts.header.label, s.oop_seat, s.ip_seat, s.pot, s.stack, s.rake_rate, s.rake_cap
    );
    let mut config = SpotConfig::for_formation("srp-btn-bb", "data/ranges")
        .unwrap_or_else(|e| die(e.to_string()));
    config.oop_range = s.oop_range;
    config.ip_range = s.ip_range;
    config.pot_bb = s.pot;
    config.stack_bb = s.stack;
    config.rake_rate = s.rake_rate;
    config.rake_cap_bb = s.rake_cap;
    config
}

/// `export-range`: bridge a solved preflop line into `solve-gen solve` inputs.
/// Ranges/pot/stack are flop-independent (they're the preflop arrival), so the
/// flop only enters the emitted command. `--hero`/`--hand` only relabel the
/// stderr provenance — the emitted args are position-derived, unchanged.
/// Nothing here links the solver — it just prints strings.
fn run_export_range(
    ruleset: &str,
    line: Option<&str>,
    flop: Option<&str>,
    hero: Option<&str>,
    hand: Option<&str>,
) {
    let charts = PreflopCharts::load(format!("data/preflop/{ruleset}")).unwrap_or_else(|e| {
        eprintln!("{e}");
        std::process::exit(2);
    });
    let die = |msg: String| -> ! {
        eprintln!("{msg}");
        std::process::exit(2);
    };

    let Some(line) = line else {
        let lines = charts.flop_lines();
        if lines.is_empty() {
            die(format!(
                "no flop-closing lines in {ruleset}'s committed charts \
                 (regenerate charts.jsonl for rarer/deeper lines)"
            ));
        }
        println!(
            "Flop-closing lines for {} — pass one to --line:",
            charts.header.label
        );
        for (l, pot) in lines {
            println!("  {l}\t(pot {pot:.1}bb)");
        }
        return;
    };

    let LineSpot {
        oop_seat,
        ip_seat,
        oop_reach,
        ip_reach,
        oop_range: oop,
        ip_range: ip,
        pot,
        stack,
        rake_rate,
        rake_cap,
    } = derive_line_spot(&charts, line).unwrap_or_else(|e| die(e));

    // Provenance on stderr; the copy-pasteable args/command on stdout.
    eprintln!("# {} — line {line}", charts.header.label);
    match hero {
        Some(h) => {
            // Which of the two flop seats is hero, and thus which side is theirs.
            let (side, hero_seat, hero_reach, villain_seat, villain_side) =
                if h.eq_ignore_ascii_case(&oop_seat) {
                    ("OOP", &oop_seat, &oop_reach, &ip_seat, "IP")
                } else if h.eq_ignore_ascii_case(&ip_seat) {
                    ("IP", &ip_seat, &ip_reach, &oop_seat, "OOP")
                } else {
                    die(format!(
                        "hero {h:?} isn't live at the flop on {line:?} \
                         ({oop_seat} is OOP, {ip_seat} is IP)"
                    ));
                };
            eprintln!(
                "# hero={hero_seat} ({side})  villain={villain_seat} ({villain_side})  \
                 pot={pot:.2}bb  eff_stack={stack:.2}bb  rake={rake_rate}/{rake_cap}bb"
            );
            eprintln!("# hero's strategy is the {side} range below.");
            if let Some(hand) = hand {
                report_hand(hand, flop, line, hero_reach);
            }
        }
        None => {
            if hand.is_some() {
                die("--hand needs --hero (whose range is the hand in?)".into());
            }
            eprintln!(
                "# OOP={oop_seat}  IP={ip_seat}  pot={pot:.2}bb  eff_stack={stack:.2}bb  \
                 rake={rake_rate}/{rake_cap}bb"
            );
        }
    }
    let args = format!(
        "--oop \"{oop}\" --ip \"{ip}\" --pot {pot:.2} --stack {stack:.2} \
         --rake-rate {rake_rate} --rake-cap {rake_cap}"
    );
    match flop {
        Some(flop) => println!("cargo run -p solve-gen --release -- solve --flop {flop} {args}"),
        None => println!("{args}"),
    }
}

/// Report (to stderr) how often hero reaches the line holding `hand`, using
/// hero's per-class arrival `reach`. Exits on a malformed hand or one that
/// collides with `flop` — an impossible holding is a user error worth catching.
fn report_hand(hand: &str, flop: Option<&str>, line: &str, reach: &[f32]) {
    let die = |msg: String| -> ! {
        eprintln!("{msg}");
        std::process::exit(2);
    };
    let cards = match parse_cards(hand).as_deref() {
        Some([a, b]) if a != b => [*a, *b],
        _ => die(format!(
            "--hand {hand:?} isn't two distinct cards, e.g. AhKh"
        )),
    };
    if let Some(flop) = flop {
        if let Some(board) = parse_cards(flop) {
            if cards.iter().any(|c| board.contains(c)) {
                die(format!(
                    "hero's hand {hand} shares a card with the flop {flop}"
                ));
            }
        }
    }
    let class = class_index(cards);
    let w = reach[class];
    eprintln!(
        "# hero holds {hand} ({}) — reaches {line} with class weight {w:.3}",
        class_name(class)
    );
    if w < 1e-3 {
        eprintln!(
            "# note: {} ~never continues this line under the equilibrium",
            class_name(class)
        );
    }
}
