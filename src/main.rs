use clap::{Args, Parser, Subcommand, ValueEnum};
use poker_trainer::postflop_table::{PostflopTable, TableNode};
use poker_trainer::preflop::{
    class_index, class_name, parse_cards, weighted_range_string, PreflopCharts,
};
use poker_trainer::solution::{formation, SolveRequest, SpotConfig};
use poker_trainer::{analyze, report, stats, trainer};
use serde::Serialize;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

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
    /// Export the reach-pruned tables' flop decision nodes as browser-ready
    /// JSONL for the web grid (data/tables-web/). Pure post-processing of local
    /// data/tables/ — links no solver, runs no solve.
    ExportTablesWeb {
        /// Source tables root (formation dirs of `solve-gen tables` output).
        #[arg(long, default_value = "data/tables")]
        tables: PathBuf,
        /// Output root — committed and staged into the site by pages.yml.
        #[arg(long, default_value = "data/tables-web")]
        out: PathBuf,
        /// Only this formation (default: every formation dir present).
        #[arg(long)]
        formation: Option<String>,
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
            None => {
                SpotConfig::for_formation(&self.formation, "data/ranges").unwrap_or_else(|e| die(e))
            }
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
        Command::ExportTablesWeb {
            tables,
            out,
            formation,
        } => run_export_tables_web(&tables, &out, formation.as_deref()),
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
/// Print `msg` to stderr and exit with status 2 — the shared "bad input, can't
/// proceed" bailout for the `export-range`/`--from` paths.
fn die(msg: impl std::fmt::Display) -> ! {
    eprintln!("{msg}");
    std::process::exit(2);
}

/// Prints and exits on a bad spec (there's nothing to solve without it).
fn grounded_config(spec: &str) -> SpotConfig {
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
    let charts = PreflopCharts::load(format!("data/preflop/{ruleset}")).unwrap_or_else(|e| die(e));

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
                die("--hand needs --hero (whose range is the hand in?)");
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

/// One flop decision node, reshaped for the web grid. `actions` is hoisted to
/// the node (it's the same for every combo — repeating it per combo doubled the
/// file), and each combo carries only its per-action `freqs`/`evs`; the browser
/// adapter re-nests these into the `data/solutions` shape `renderGrid()` reads.
/// `line`/`hero_oop` drive the node picker.
#[derive(Serialize)]
struct WebNode {
    /// Action line from the flop root, e.g. `["Check","Bet 2.0bb"]` (`[]` = root).
    line: Vec<String>,
    /// True if the acting seat is out of position.
    hero_oop: bool,
    label: String,
    villain_action: String,
    pot_bb: f32,
    board: Vec<String>,
    /// The acting player's action labels (shared by every combo below).
    actions: Vec<String>,
    strategies: Vec<WebCombo>,
}

/// One hero combo's strategy at a node: `freqs`/`evs` parallel to the node's
/// `actions` (same names as [`poker_trainer::tree::TreeNode`]).
#[derive(Serialize)]
struct WebCombo {
    hand: String,
    freqs: Vec<f32>,
    evs: Vec<f32>,
}

/// A flop entry in the web index: the filename `stem` plus a display flop.
#[derive(Serialize)]
struct WebFlop {
    stem: String,
    display: String,
}

/// One formation's web tables: its config hash (in every filename) + its flops.
#[derive(Serialize)]
struct WebFormation {
    hash: String,
    flops: Vec<WebFlop>,
}

/// `export-tables-web`: reshape the flop decision nodes of every reach-pruned
/// table under `tables` into browser-ready JSONL under `out`, plus an
/// `index.json` catalog. Pure file post-processing — no solver, no solve.
fn run_export_tables_web(tables: &Path, out: &Path, only: Option<&str>) {
    let mut dirs: Vec<PathBuf> = fs::read_dir(tables)
        .unwrap_or_else(|e| die(format!("{}: {e}", tables.display())))
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.is_dir())
        .collect();
    dirs.sort();

    let mut index: BTreeMap<String, WebFormation> = BTreeMap::new();
    for dir in dirs {
        let formation = dir.file_name().unwrap().to_string_lossy().into_owned();
        if only.is_some_and(|o| o != formation) {
            continue;
        }
        let Some(hash) = header_hash(&dir) else {
            eprintln!("skip {formation}: no header-<hash>.json");
            continue;
        };
        let out_dir = out.join(&formation);
        fs::create_dir_all(&out_dir).unwrap_or_else(|e| die(format!("{}: {e}", out_dir.display())));

        let mut flops = Vec::new();
        let mut nodes_written = 0usize;
        for stem in flop_stems(&dir, &hash) {
            let table = match PostflopTable::load(&dir, &stem, &hash) {
                Ok(t) => t,
                Err(e) => {
                    eprintln!("skip {formation}/{stem}: {e}");
                    continue;
                }
            };
            // Flop decision nodes only: 3-card board, a player (not chance/terminal).
            let mut rows: Vec<WebNode> = table
                .nodes()
                .filter(|tn| {
                    tn.node.board.len() == 3 && matches!(tn.node.player.as_str(), "oop" | "ip")
                })
                .map(|tn| web_node(&formation, &stem, tn))
                .collect();
            // Stable, logical order: root first, then by depth then label.
            rows.sort_by(|a, b| (a.line.len(), &a.line).cmp(&(b.line.len(), &b.line)));
            nodes_written += rows.len();
            let body = rows
                .iter()
                .map(|r| serde_json::to_string(r).unwrap())
                .collect::<Vec<_>>()
                .join("\n");
            let file = out_dir.join(format!("{stem}-{hash}.jsonl"));
            fs::write(&file, body).unwrap_or_else(|e| die(format!("{}: {e}", file.display())));
            flops.push(WebFlop {
                display: titlecase_flop(&stem),
                stem,
            });
        }
        flops.sort_by(|a, b| a.stem.cmp(&b.stem));
        eprintln!(
            "{formation}: {} flops, {nodes_written} flop nodes",
            flops.len()
        );
        index.insert(formation, WebFormation { hash, flops });
    }

    if index.is_empty() {
        die(format!("no tables found under {}", tables.display()));
    }
    fs::create_dir_all(out).unwrap_or_else(|e| die(format!("{}: {e}", out.display())));
    let index_path = out.join("index.json");
    fs::write(&index_path, serde_json::to_string_pretty(&index).unwrap())
        .unwrap_or_else(|e| die(format!("{}: {e}", index_path.display())));
    eprintln!("wrote {} formations to {}", index.len(), out.display());
}

/// The `<hash>` of `<dir>/header-<hash>.json` (first one, if several).
fn header_hash(dir: &Path) -> Option<String> {
    let mut hashes: Vec<String> = fs::read_dir(dir)
        .ok()?
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let name = e.file_name().into_string().ok()?;
            name.strip_prefix("header-")?
                .strip_suffix(".json")
                .map(str::to_string)
        })
        .collect();
    hashes.sort();
    hashes.into_iter().next()
}

/// Flop filename stems in `dir` for config `hash`: `<stem>-<hash>.jsonl` → `<stem>`.
fn flop_stems(dir: &Path, hash: &str) -> Vec<String> {
    let suffix = format!("-{hash}.jsonl");
    let mut stems: Vec<String> = fs::read_dir(dir)
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            e.file_name()
                .into_string()
                .ok()?
                .strip_suffix(&suffix)
                .map(str::to_string)
        })
        .collect();
    stems.sort();
    stems
}

/// Reshape one stored table node into a [`WebNode`] — the load-bearing bit is
/// transposing `freqs`/`evs` from `[action][hand]` to per-hand strategies.
fn web_node(formation_id: &str, stem: &str, tn: &TableNode) -> WebNode {
    let n = &tn.node;
    let hero_oop = n.player == "oop";
    let f = formation(formation_id);
    let label_prefix = f.map_or(formation_id, |f| f.label);
    let seat = match f {
        Some(f) if hero_oop => f.oop_seat,
        Some(f) => f.ip_seat,
        None if hero_oop => "OOP",
        None => "IP",
    };
    let villain_action = if n.line.is_empty() {
        format!("{seat} to act (first decision)")
    } else {
        format!("after {} — {seat} to act", n.line.join(", "))
    };
    let strategies = n
        .hands
        .iter()
        .enumerate()
        .map(|(j, hand)| WebCombo {
            hand: hand.clone(),
            freqs: n.freqs.iter().map(|per_action| per_action[j]).collect(),
            evs: n.evs.iter().map(|per_action| per_action[j]).collect(),
        })
        .collect();
    WebNode {
        line: n.line.clone(),
        hero_oop,
        label: format!("{label_prefix}, {}", titlecase_flop(stem)),
        villain_action,
        pot_bb: n.pot_bb,
        board: n.board.clone(),
        actions: n.actions.clone(),
        strategies,
    }
}

/// A lowercase flop filename stem → display flop: `"td9d6h"` → `"Td9d6h"`.
/// Only rank letters (t,j,q,k,a) uppercase; suit letters (c,d,h,s) stay lower.
fn titlecase_flop(stem: &str) -> String {
    stem.chars()
        .map(|c| {
            if "tjqka".contains(c) {
                c.to_ascii_uppercase()
            } else {
                c
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use poker_trainer::tree::TreeNode;

    #[test]
    fn web_node_transposes_and_labels() {
        let tn = TableNode {
            reach: 1.0,
            node: TreeNode {
                player: "oop".into(),
                board: vec!["6h".into(), "9d".into(), "Td".into()],
                pot_bb: 6.0,
                line: vec![],
                actions: vec!["Check".into(), "Bet 2.0bb".into()],
                dealable: vec![],
                hands: vec!["AsKs".into(), "QdQc".into()],
                // [action][hand]: Check freqs per hand, then Bet freqs per hand.
                freqs: vec![vec![0.6, 0.3], vec![0.4, 0.7]],
                evs: vec![vec![1.0, 2.0], vec![1.5, 2.4]],
                weights: vec![1.0, 1.0],
                equity: vec![0.55, 0.6],
            },
        };
        let w = web_node("srp-btn-bb", "td9d6h", &tn);

        // Per-hand transpose: hand j gets column j from each action row.
        assert_eq!(w.actions, vec!["Check", "Bet 2.0bb"]);
        assert_eq!(w.strategies[0].freqs, vec![0.6, 0.4]);
        assert_eq!(w.strategies[0].evs, vec![1.0, 1.5]);
        assert_eq!(w.strategies[1].freqs, vec![0.3, 0.7]);
        assert_eq!(w.strategies[1].evs, vec![2.0, 2.4]);

        assert!(w.hero_oop);
        assert_eq!(w.label, "SRP BTN vs BB, Td9d6h");
        assert_eq!(w.villain_action, "BB to act (first decision)");
    }

    #[test]
    fn titlecase_flop_uppercases_ranks_not_suits() {
        assert_eq!(titlecase_flop("td9d6h"), "Td9d6h");
        assert_eq!(titlecase_flop("ahad8c"), "AhAd8c");
        assert_eq!(titlecase_flop("4s3s2d"), "4s3s2d");
    }
}
