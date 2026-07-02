//! Offline GTO solution generator (AGPL — links postflop-solver).
//!
//! Walks a manifest of (formation × flop set) entries, solves each spot to
//! equilibrium, navigates to the hero decision nodes (OOP checks, IP c-bets,
//! hero faces the bet), and dumps the per-hand action mix + per-action EV as
//! `data/solutions/<flop>-<confighash8>-<node>.json` with the full
//! [`SpotConfig`] and provenance embedded. The trainer reads those files and
//! never links this crate.

use clap::{Args, Parser, Subcommand};
use poker_trainer::solution::{
    formation, GenInfo, HandStrategy, NodeStrategy, SolveRequest, SolvedSpot, SpotConfig,
};
use poker_trainer::tree::TreeNode;
use postflop_solver::*;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::fs;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

const CHIPS_PER_BB: f32 = 100.0;

/// One spot to solve: a flop plus the full game config.
#[derive(Debug)]
struct Spot {
    label: String,
    flop: String,
    config: SpotConfig,
}

#[derive(Parser)]
#[command(name = "solve-gen", about = "Offline GTO solution generator")]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Walk a manifest, solving every spot whose config-hash isn't already in
    /// the output dir (resumable; default manifest: manifests/starter-8.toml).
    Gen {
        #[arg(long)]
        manifest: Option<PathBuf>,
    },
    /// Solve one custom spot and write its JSON into the solution dir.
    Solve(SolveArgs),
    /// Tree-session server: solve a spot, keep it resident, and answer
    /// line-delimited JSON node queries on stdio (protocol v2, design doc 01).
    Serve,
}

#[derive(Args)]
struct SolveArgs {
    /// Flop as rs_poker cards, e.g. `Td9d6h`.
    #[arg(long)]
    flop: String,
    /// Formation id; supplies seats, default pot/stacks, and the ranges read
    /// from data/ranges/<formation>/{oop,ip}.txt.
    #[arg(long, default_value = "srp-btn-bb")]
    formation: String,
    /// OOP range override.
    #[arg(long)]
    oop: Option<String>,
    /// IP range override.
    #[arg(long)]
    ip: Option<String>,
    /// Flop bet sizes, e.g. `"33%, 75%"`.
    #[arg(long)]
    sizes: Option<String>,
    /// Turn bet sizes.
    #[arg(long)]
    turn_sizes: Option<String>,
    /// River bet sizes.
    #[arg(long)]
    river_sizes: Option<String>,
    /// Effective stack in bb.
    #[arg(long)]
    stack: Option<f32>,
    /// Starting pot in bb.
    #[arg(long)]
    pot: Option<f32>,
    /// Rake rate (0.05 = 5%).
    #[arg(long)]
    rake_rate: Option<f32>,
    /// Rake cap in bb.
    #[arg(long)]
    rake_cap: Option<f32>,
    /// Output directory (defaults to the repo's data/solutions).
    #[arg(long)]
    out: Option<PathBuf>,
}

/// A path relative to the repo root (this crate lives two levels down).
fn repo_path(rel: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(rel)
}

fn main() {
    let die = |e: String| -> ! {
        eprintln!("{e}");
        std::process::exit(2);
    };
    match Cli::parse()
        .command
        .unwrap_or(Command::Gen { manifest: None })
    {
        Command::Gen { manifest } => {
            let path = manifest.unwrap_or_else(|| repo_path("manifests/starter-8.toml"));
            let spots = load_manifest(&path).unwrap_or_else(|e| die(e));
            write_all(&spots, &repo_path("data/solutions"));
        }
        Command::Solve(a) => {
            let out = a.out.clone().unwrap_or_else(|| repo_path("data/solutions"));
            let spot = spot_from_args(&a).unwrap_or_else(|e| die(e));
            write_all(std::slice::from_ref(&spot), &out);
        }
        Command::Serve => serve(),
    }
}

/// "SRP BTN vs BB, Td9d6h" — formation label + flop.
fn spot_label(formation_id: &str, flop: &str) -> String {
    let label = formation(formation_id).map_or(formation_id, |f| f.label);
    format!("{label}, {flop}")
}

/// The formation's seat names for node labels; a config from an unknown
/// formation (hand-edited files) still solves, just with generic seats.
fn seats(formation_id: &str) -> (&'static str, &'static str) {
    formation(formation_id).map_or(("OOP", "IP"), |f| (f.oop_seat, f.ip_seat))
}

fn spot_from_args(a: &SolveArgs) -> Result<Spot, String> {
    let mut c = SpotConfig::for_formation(&a.formation, repo_path("data/ranges"))
        .map_err(|e| e.to_string())?;
    let overrides = [
        (&mut c.oop_range, &a.oop),
        (&mut c.ip_range, &a.ip),
        (&mut c.flop_sizes, &a.sizes),
        (&mut c.turn_sizes, &a.turn_sizes),
        (&mut c.river_sizes, &a.river_sizes),
    ];
    for (field, value) in overrides {
        if let Some(v) = value {
            *field = v.clone();
        }
    }
    if let Some(v) = a.stack {
        c.stack_bb = v;
    }
    if let Some(v) = a.pot {
        c.pot_bb = v;
    }
    if let Some(v) = a.rake_rate {
        c.rake_rate = v;
    }
    if let Some(v) = a.rake_cap {
        c.rake_cap_bb = v;
    }
    Ok(Spot {
        label: spot_label(&c.formation, &a.flop),
        flop: a.flop.clone(),
        config: c,
    })
}

/// A manifest: named flop sets plus runs of (formation × flop set ×
/// config overrides) — design doc 02.
#[derive(Deserialize)]
struct Manifest {
    #[serde(default)]
    flopsets: BTreeMap<String, Vec<String>>,
    runs: Vec<Run>,
}

#[derive(Deserialize)]
struct Run {
    formation: String,
    /// A `[flopsets]` key, or the built-in `"all-iso-flops"`.
    flops: String,
    flop_sizes: Option<String>,
    turn_sizes: Option<String>,
    river_sizes: Option<String>,
    stack_bb: Option<f32>,
    pot_bb: Option<f32>,
    rake_rate: Option<f32>,
    rake_cap_bb: Option<f32>,
}

fn load_manifest(path: &Path) -> Result<Vec<Spot>, String> {
    let text = fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let m: Manifest = toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    manifest_spots(&m)
}

fn manifest_spots(m: &Manifest) -> Result<Vec<Spot>, String> {
    let mut spots = Vec::new();
    for run in &m.runs {
        let mut c = SpotConfig::for_formation(&run.formation, repo_path("data/ranges"))
            .map_err(|e| e.to_string())?;
        let overrides = [
            (&mut c.flop_sizes, &run.flop_sizes),
            (&mut c.turn_sizes, &run.turn_sizes),
            (&mut c.river_sizes, &run.river_sizes),
        ];
        for (field, value) in overrides {
            if let Some(v) = value {
                *field = v.clone();
            }
        }
        if let Some(v) = run.stack_bb {
            c.stack_bb = v;
        }
        if let Some(v) = run.pot_bb {
            c.pot_bb = v;
        }
        if let Some(v) = run.rake_rate {
            c.rake_rate = v;
        }
        if let Some(v) = run.rake_cap_bb {
            c.rake_cap_bb = v;
        }
        let flops = if run.flops == "all-iso-flops" {
            iso_flops()
        } else {
            m.flopsets
                .get(&run.flops)
                .ok_or_else(|| format!("unknown flop set {:?}", run.flops))?
                .clone()
        };
        for flop in flops {
            spots.push(Spot {
                label: spot_label(&c.formation, &flop),
                flop,
                config: c.clone(),
            });
        }
    }
    Ok(spots)
}

/// All 1,755 suit-isomorphic flops (22,100 raw flops / suit symmetry), as
/// solver-ready strings like `"2c2d2h"`. Canonical form = the smallest card-id
/// triple under the 24 suit permutations.
fn iso_flops() -> Vec<String> {
    let mut perms: Vec<[u8; 4]> = Vec::new();
    for a in 0..4u8 {
        for b in 0..4u8 {
            for c in 0..4u8 {
                for d in 0..4u8 {
                    if a != b && a != c && a != d && b != c && b != d && c != d {
                        perms.push([a, b, c, d]);
                    }
                }
            }
        }
    }
    let mut seen = std::collections::BTreeSet::new();
    for x in 0..52u8 {
        for y in (x + 1)..52 {
            for z in (y + 1)..52 {
                let canon = perms
                    .iter()
                    .map(|p| {
                        let mut f = [x, y, z].map(|c| (c & !3) | p[(c & 3) as usize]);
                        f.sort_unstable();
                        f
                    })
                    .min()
                    .unwrap();
                seen.insert(canon);
            }
        }
    }
    seen.into_iter()
        .map(|f| f.map(|c| card_to_string(c).unwrap()).join(""))
        .collect()
}

/// The solved game held by `serve`, plus what the game doesn't track for us:
/// the display labels of the line walked so far and the snapshot provenance.
struct ServeSession {
    game: PostFlopGame,
    starting_pot: i32,
    labels: Vec<String>,
    config: SpotConfig,
    generator: GenInfo,
    /// P10 node locks: `(node history, action-major strategy)`. `resolve`
    /// re-applies the whole set on a fresh allocation, so it stays the source of
    /// truth regardless of what `allocate_memory` does to prior locks.
    locks: Vec<(Vec<usize>, Vec<f32>)>,
}

/// `serve`: one JSON request per stdin line, one JSON response per stdout line.
/// All human output (solve progress) goes to stderr — stdout is protocol-only.
fn serve() {
    let mut sess: Option<ServeSession> = None;
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        let (resp, quit) = respond(&mut sess, &line);
        // stdout is block-buffered when piped; flush or the trainer hangs.
        writeln!(stdout, "{resp}").unwrap();
        stdout.flush().unwrap();
        if quit {
            break;
        }
    }
}

/// Handle one request line; returns the response JSON and whether to exit.
/// Pure in/out (no I/O), so the dispatch is unit-testable without a solve.
fn respond(sess: &mut Option<ServeSession>, line: &str) -> (Value, bool) {
    let req: Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(e) => return (json!({"error": format!("bad JSON: {e}")}), false),
    };
    let quit = req.get("op").and_then(Value::as_str) == Some("quit");
    let mut resp = handle_op(sess, &req).unwrap_or_else(|e| json!({ "error": e }));
    if let (Some(id), Some(obj)) = (req.get("id"), resp.as_object_mut()) {
        obj.insert("id".into(), id.clone());
    }
    (resp, quit)
}

fn handle_op(sess: &mut Option<ServeSession>, req: &Value) -> Result<Value, String> {
    let op = req
        .get("op")
        .and_then(Value::as_str)
        .ok_or("missing \"op\"")?;
    match op {
        "quit" => return Ok(json!({"ok": true})),
        "solve" => {
            if let Some(v) = req.get("v").and_then(Value::as_u64) {
                if v != 2 {
                    return Err(format!("unsupported protocol v{v}; this serve speaks v2"));
                }
            }
            let config = req.get("config").ok_or("solve needs \"config\"")?;
            let r: SolveRequest =
                serde_json::from_value(config.clone()).map_err(|e| format!("bad config: {e}"))?;
            let spot = Spot {
                label: spot_label(&r.config.formation, &r.flop),
                flop: r.flop,
                config: r.config,
            };
            let (game, exploitability, cached) = load_or_solve(&spot)?;
            let s = sess.insert(ServeSession {
                game,
                starting_pot: (spot.config.pot_bb * CHIPS_PER_BB) as i32,
                labels: Vec::new(),
                config: spot.config,
                generator: gen_info(exploitability),
                locks: Vec::new(),
            });
            let mut ack = node_payload(s);
            ack["cached"] = json!(cached);
            return Ok(ack);
        }
        // Validate the op name before requiring a game, so an unknown op is
        // reported as such even before the first solve.
        "node" | "play" | "deal" | "back" | "root" | "snapshot" | "runouts" | "lock"
        | "resolve" => {}
        other => return Err(format!("unknown op {other:?}")),
    }
    let s = sess.as_mut().ok_or("no game held — send op:solve first")?;
    match op {
        "node" => {}
        "runouts" => return runouts_payload(s),
        "play" => {
            let i = req
                .get("action")
                .and_then(Value::as_u64)
                .ok_or("play needs \"action\" (an index)")? as usize;
            if s.game.is_terminal_node() || s.game.is_chance_node() {
                return Err("not a player node — use op:deal at chance nodes".into());
            }
            let actions = s.game.available_actions();
            if i >= actions.len() {
                return Err(format!(
                    "action {i} out of range ({} actions)",
                    actions.len()
                ));
            }
            let label = fmt_action(&actions[i]);
            s.game.play(i);
            s.labels.push(label);
        }
        "deal" => {
            let card_str = req
                .get("card")
                .and_then(Value::as_str)
                .ok_or("deal needs \"card\" (e.g. \"7h\")")?;
            if !s.game.is_chance_node() {
                return Err("not a chance node".into());
            }
            let card = card_from_str(card_str)?;
            if s.game.possible_cards() & (1u64 << card) == 0 {
                return Err(format!("{card_str} can't be dealt here"));
            }
            s.game.play(card as usize);
            s.labels.push(format!("deal {card_str}"));
        }
        "back" => {
            // history stores card IDs at chance nodes, so replaying it is exact.
            let mut history = s.game.history().to_vec();
            if history.pop().is_some() {
                s.game.apply_history(&history);
                s.labels.pop();
            }
        }
        "root" => {
            s.game.back_to_root();
            s.labels.clear();
        }
        "lock" => {
            // Record the current player node's forced strategy; `resolve`
            // applies it. Trainer sends `[action][hand]` (parallel to a node's
            // `freqs`); the solver wants action-major flat, so flattening the
            // rows in order is exactly `lock_current_strategy`'s layout.
            if s.game.is_terminal_node() || s.game.is_chance_node() {
                return Err("lock needs a player node".into());
            }
            let player = s.game.current_player();
            let n_actions = s.game.available_actions().len();
            let n_hands = s.game.private_cards(player).len();
            let rows = req
                .get("strategy")
                .and_then(Value::as_array)
                .ok_or("lock needs \"strategy\": [action][hand]")?;
            if rows.len() != n_actions {
                return Err(format!(
                    "strategy has {} action rows, node has {n_actions}",
                    rows.len()
                ));
            }
            let mut flat = Vec::with_capacity(n_actions * n_hands);
            for row in rows {
                let vals = row.as_array().ok_or("strategy rows must be arrays")?;
                if vals.len() != n_hands {
                    return Err(format!(
                        "strategy row has {} hands, node has {n_hands}",
                        vals.len()
                    ));
                }
                for v in vals {
                    flat.push(v.as_f64().ok_or("strategy values must be numbers")? as f32);
                }
            }
            let hist = s.game.history().to_vec();
            s.locks.retain(|(h, _)| h != &hist); // one lock per node; replace
            s.locks.push((hist, flat));
        }
        "resolve" => {
            // Re-solve from scratch with every lock held. Not a warm start
            // (allocate_memory zeros the strategy) — honest re-solve, ~as costly
            // as the original. ponytail: warm-start if resolve latency bites.
            let here = s.game.history().to_vec();
            let target = s.starting_pot as f32 * 0.005;
            let locks = s.locks.clone(); // borrow s.game mutably in the loop
            s.game.allocate_memory(false);
            for (hist, strat) in &locks {
                s.game.apply_history(hist);
                s.game.lock_current_strategy(strat);
                s.game.back_to_root();
            }
            eprintln!("resolving with {} lock(s)…", locks.len());
            let exploitability = solve(&mut s.game, 1000, target, false);
            s.generator = gen_info(exploitability);
            s.game.apply_history(&here); // back to where the user was
        }
        "snapshot" => {
            if s.game.is_terminal_node() || s.game.is_chance_node() {
                return Err("snapshot needs a player node".into());
            }
            let node = node_payload_parts(s);
            let label = req
                .get("label")
                .and_then(Value::as_str)
                .map(str::to_string)
                .unwrap_or_else(|| node.line.join(" · "));
            let hero_oop = s.game.current_player() == 0;
            let villain_action = s.labels.last().cloned().unwrap_or_default();
            let (config, generator) = (s.config.clone(), s.generator.clone());
            let spot = extract(
                &mut s.game,
                label,
                node.board,
                node.pot_bb,
                hero_oop,
                villain_action,
                &config,
                &generator,
            );
            return Ok(serde_json::to_value(spot).unwrap());
        }
        _ => unreachable!("op validated above"),
    }
    Ok(node_payload(s))
}

fn node_payload(s: &mut ServeSession) -> Value {
    serde_json::to_value(node_payload_parts(s)).unwrap()
}

/// The current node as the protocol's [`TreeNode`] payload.
fn node_payload_parts(s: &mut ServeSession) -> TreeNode {
    let game = &mut s.game;
    let board = game
        .current_board()
        .iter()
        .map(|&c| card_to_string(c).unwrap())
        .collect();
    let bets = game.total_bet_amount();
    let pot_bb = (s.starting_pot + bets[0] + bets[1]) as f32 / CHIPS_PER_BB;
    let line = s.labels.clone();
    let base = TreeNode {
        board,
        pot_bb,
        line,
        ..Default::default()
    };

    if game.is_terminal_node() {
        return TreeNode {
            player: "terminal".into(),
            ..base
        };
    }
    if game.is_chance_node() {
        let mask = game.possible_cards();
        return TreeNode {
            player: "chance".into(),
            dealable: (0u8..52)
                .filter(|&c| mask & (1u64 << c) != 0)
                .map(|c| card_to_string(c).unwrap())
                .collect(),
            ..base
        };
    }

    let player = game.current_player();
    game.cache_normalized_weights();
    let actions: Vec<String> = game.available_actions().iter().map(fmt_action).collect();
    let hands = holes_to_strings(game.private_cards(player)).unwrap();
    let n = hands.len();
    let strat = game.strategy(); // [action * n + hand]
    let evs = game.expected_values_detail(player); // chips
    TreeNode {
        player: if player == 0 { "oop" } else { "ip" }.into(),
        actions: actions.clone(),
        hands,
        freqs: (0..actions.len())
            .map(|i| strat[i * n..(i + 1) * n].to_vec())
            .collect(),
        evs: (0..actions.len())
            .map(|i| {
                evs[i * n..(i + 1) * n]
                    .iter()
                    .map(|&x| x / CHIPS_PER_BB)
                    .collect()
            })
            .collect(),
        weights: game.normalized_weights(player).to_vec(),
        equity: game.equity(player),
        ..base
    }
}

/// The `runouts` op: per dealable card at a chance node, deal it, read the
/// next player's reach-weighted aggregate action mix and EV, and step back.
fn runouts_payload(s: &mut ServeSession) -> Result<Value, String> {
    if !s.game.is_chance_node() {
        return Err("not a chance node".into());
    }
    let history = s.game.history().to_vec();
    let mask = s.game.possible_cards();
    let mut list = Vec::new();
    for card in (0u8..52).filter(|&c| mask & (1u64 << c) != 0) {
        s.game.play(card as usize);
        s.game.cache_normalized_weights();
        let player = s.game.current_player();
        let weights = s.game.normalized_weights(player);
        let strat = s.game.strategy(); // [action * n + hand]
        let evs = s.game.expected_values(player); // chips, per hand
        let n = weights.len();
        // Zero-reach line (nothing check-checks here, say): fall back to the
        // plain mean so the runout still shows the strategy's shape.
        let wsum: f32 = weights.iter().sum();
        let reached = wsum > 1e-9;
        let w = |j: usize| if reached { weights[j] } else { 1.0 };
        let div = if reached { wsum } else { n.max(1) as f32 };
        let actions: Vec<String> = s.game.available_actions().iter().map(fmt_action).collect();
        let freqs: Vec<f32> = (0..actions.len())
            .map(|a| (0..n).map(|j| w(j) * strat[a * n + j]).sum::<f32>() / div)
            .collect();
        let ev_bb = (0..n).map(|j| w(j) * evs[j]).sum::<f32>() / div / CHIPS_PER_BB;
        list.push(json!({
            "card": card_to_string(card).unwrap(),
            "actions": actions,
            "freqs": freqs,
            "ev_bb": ev_bb,
        }));
        s.game.apply_history(&history);
    }
    Ok(json!({ "runouts": list }))
}

fn write_all(spots: &[Spot], out_dir: &Path) {
    fs::create_dir_all(out_dir).unwrap();
    for spot in spots {
        let stem = format!("{}-{}", spot.flop.to_lowercase(), spot.config.hash8());
        // Resumable: a run that died mid-manifest picks up where it left off.
        if out_dir.join(format!("{stem}-ip.json")).exists() {
            println!("Cached, skipping: {} ({stem})", spot.label);
            continue;
        }
        println!("Solving: {}", spot.label);
        // One solved game yields the IP c-bet node plus one OOP defend node per
        // c-bet size; solve_spot hands back each with its own file stem.
        for (file_stem, solved) in solve_spot(spot, &stem) {
            let file = out_dir.join(format!("{file_stem}.json"));
            fs::write(&file, serde_json::to_string_pretty(&solved).unwrap()).unwrap();
            println!(
                "  -> {} ({} hero hands)",
                file.display(),
                solved.strategies.len()
            );
        }
    }
}

fn gen_info(exploitability_chips: f32) -> GenInfo {
    GenInfo {
        version: env!("CARGO_PKG_VERSION").into(),
        exploitability_bb: exploitability_chips / CHIPS_PER_BB,
    }
}

/// Build and solve one spot's game. Errors are strings from the solver's own
/// validation (bad range/size/flop), so `serve` can report them over the
/// protocol instead of dying. Progress prints on stderr. Returns the solved
/// game and the exploitability reached, in chips.
/// Config-hash solve cache (design 01 M3): solver-native saves under
/// `~/.cache/poker-trainer/solves/<flop>-<hash8>.bin`. AGPL-side detail —
/// the trainer never reads these. Exploitability rides in the save's memo.
fn solve_cache_path(spot: &Spot) -> Option<PathBuf> {
    let cache = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))?;
    let dir = cache.join("poker-trainer/solves");
    fs::create_dir_all(&dir).ok()?;
    Some(dir.join(format!(
        "{}-{}.bin",
        spot.flop.to_lowercase(),
        spot.config.hash8()
    )))
}

/// Load a cached solve if present, else solve and cache. The bool is
/// `cached` for the serve ack. A corrupt/unreadable cache file just re-solves
/// and overwrites; a failed save only warns — the cache is an optimization.
fn load_or_solve(spot: &Spot) -> Result<(PostFlopGame, f32, bool), String> {
    let path = solve_cache_path(spot);
    if let Some(p) = &path {
        if let Ok((mut game, memo)) = load_data_from_file::<PostFlopGame, _>(p, None) {
            eprintln!("  loaded cached solve {}", p.display());
            game.back_to_root();
            return Ok((game, memo.parse().unwrap_or(f32::NAN), true));
        }
    }
    let (game, exploitability) = build_and_solve(spot)?;
    if let Some(p) = &path {
        if let Err(e) = save_data_to_file(&game, &exploitability.to_string(), p, None) {
            eprintln!("  warning: failed to cache solve: {e}");
        }
    }
    Ok((game, exploitability, false))
}

fn build_and_solve(spot: &Spot) -> Result<(PostFlopGame, f32), String> {
    let c = &spot.config;
    let starting_pot = (c.pot_bb * CHIPS_PER_BB) as i32;
    let card_config = CardConfig {
        range: [c.oop_range.parse()?, c.ip_range.parse()?],
        flop: flop_from_str(&spot.flop)?,
        turn: NOT_DEALT,
        river: NOT_DEALT,
    };
    let flop_bets = BetSizeOptions::try_from((c.flop_sizes.as_str(), "2.5x"))?;
    let turn_bets = BetSizeOptions::try_from((c.turn_sizes.as_str(), "2.5x"))?;
    let river_bets = BetSizeOptions::try_from((c.river_sizes.as_str(), "2.5x"))?;
    let tree_config = TreeConfig {
        initial_state: BoardState::Flop,
        starting_pot,
        effective_stack: (c.stack_bb * CHIPS_PER_BB) as i32,
        rake_rate: f64::from(c.rake_rate),
        rake_cap: f64::from(c.rake_cap_bb * CHIPS_PER_BB),
        flop_bet_sizes: [flop_bets.clone(), flop_bets],
        turn_bet_sizes: [turn_bets.clone(), turn_bets],
        river_bet_sizes: [river_bets.clone(), river_bets],
        turn_donk_sizes: None,
        river_donk_sizes: None,
        add_allin_threshold: 1.5,
        force_allin_threshold: 0.15,
        merging_threshold: 0.1,
    };

    let action_tree = ActionTree::new(tree_config)?;
    let mut game = PostFlopGame::with_config(card_config, action_tree)?;
    game.allocate_memory(false);
    let target = starting_pot as f32 * 0.005; // 0.5% of pot
    let exploitability = solve(&mut game, 1000, target, false);
    eprintln!(
        "  exploitability: {:.3} chips ({:.3}bb)",
        exploitability,
        exploitability / CHIPS_PER_BB
    );
    Ok((game, exploitability))
}

fn solve_spot(spot: &Spot, stem: &str) -> Vec<(String, SolvedSpot)> {
    let (game, exploitability, _) = load_or_solve(spot).expect("spot must solve");
    let mut game = game;
    let generator = gen_info(exploitability);
    let starting_pot = (spot.config.pot_bb * CHIPS_PER_BB) as i32;
    let pot_bb = starting_pot as f32 / CHIPS_PER_BB;
    let (oop_seat, ip_seat) = seats(&spot.config.formation);
    let board: Vec<String> = flop_from_str(&spot.flop)
        .unwrap()
        .iter()
        .map(|&c| card_to_string(c).unwrap())
        .collect();

    // All decision nodes come from this one solved game. Navigate: OOP checks,
    // IP decides whether to c-bet, then OOP faces each c-bet size — one defend
    // node per size for a symmetric library.
    let to_cbet = |game: &mut PostFlopGame| {
        game.back_to_root();
        assert_eq!(game.current_player(), 0, "root should be OOP");
        game.play(action_index(game, |a| matches!(a, Action::Check)));
        assert_eq!(
            game.current_player(),
            1,
            "after check, IP (hero) decides whether to c-bet"
        );
    };

    let mut out = Vec::new();

    // Node 1: hero is IP, villain has checked — c-bet or check back?
    to_cbet(&mut game);
    let bet_indices: Vec<usize> = game
        .available_actions()
        .iter()
        .enumerate()
        .filter(|(_, a)| matches!(a, Action::Bet(_)))
        .map(|(i, _)| i)
        .collect();
    assert!(
        !bet_indices.is_empty(),
        "c-bet node offers no bet (bet-size config too narrow?)"
    );
    out.push((
        format!("{stem}-ip"),
        extract(
            &mut game,
            format!(
                "{} — you're {ip_seat}, {oop_seat} checks: c-bet?",
                spot.label
            ),
            board.clone(),
            pot_bb,
            false,
            format!("Villain ({oop_seat}) checks to you"),
            &spot.config,
            &generator,
        ),
    ));

    // Defend node per c-bet size: descend into each bet, extract, re-navigate.
    for &bi in &bet_indices {
        let bet_chips = match game.available_actions()[bi] {
            Action::Bet(c) => c,
            _ => unreachable!(),
        };
        let pct = (100.0 * bet_chips as f32 / starting_pot as f32).round() as i32;
        let bet_bb = bet_chips as f32 / CHIPS_PER_BB;
        game.play(bi);
        assert_eq!(game.current_player(), 0, "hero (OOP) faces the bet");
        out.push((
            format!("{stem}-oop-{pct}"),
            extract(
                &mut game,
                format!(
                    "{} — you're {oop_seat}, facing {ip_seat} {pct}% c-bet: defend?",
                    spot.label
                ),
                board.clone(),
                pot_bb,
                true,
                format!("You check, villain bets {bet_bb:.1}bb ({pct}% pot)"),
                &spot.config,
                &generator,
            ),
        ));
        to_cbet(&mut game); // reset to the c-bet node for the next size
    }

    out
}

/// Build a [`SolvedSpot`] from the game positioned at the hero's decision node.
/// Node-specific bits (`label`, `hero_oop`, `villain_action`) are passed in; the
/// strategy/EV read off `current_player()` is the same for any node.
#[allow(clippy::too_many_arguments)] // a plain constructor; a params struct would just rename these
fn extract(
    game: &mut PostFlopGame,
    label: String,
    board: Vec<String>,
    pot_bb: f32,
    hero_oop: bool,
    villain_action: String,
    config: &SpotConfig,
    generator: &GenInfo,
) -> SolvedSpot {
    game.cache_normalized_weights();
    let hero = game.current_player();
    let actions = game.available_actions();
    let labels: Vec<String> = actions.iter().map(fmt_action).collect();

    let cards = game.private_cards(hero);
    let n = cards.len();
    let hands = holes_to_strings(cards).unwrap();
    let strat = game.strategy(); // [action * n + hand]
    let evs = game.expected_values_detail(hero); // chips, [action * n + hand]

    let strategies: Vec<HandStrategy> = (0..n)
        .map(|j| HandStrategy {
            hand: hands[j].clone(),
            strategy: NodeStrategy {
                actions: labels.clone(),
                frequencies: (0..actions.len()).map(|i| strat[i * n + j]).collect(),
                action_ev: (0..actions.len())
                    .map(|i| evs[i * n + j] / CHIPS_PER_BB)
                    .collect(),
            },
        })
        .collect();
    assert!(!strategies.is_empty(), "extracted node has no hero hands");

    SolvedSpot {
        label,
        board,
        pot_bb,
        hero_oop,
        villain_action,
        config: Some(config.clone()),
        generator: Some(generator.clone()),
        strategies,
    }
}

fn action_index(game: &PostFlopGame, pred: impl Fn(&Action) -> bool) -> usize {
    game.available_actions()
        .iter()
        .position(pred)
        .expect("expected action not available")
}

/// postflop-solver `Action` -> a trainer-facing label, bet amounts in bb.
fn fmt_action(a: &Action) -> String {
    let bb = |c: i32| c as f32 / CHIPS_PER_BB;
    match a {
        Action::Fold => "Fold".into(),
        Action::Check => "Check".into(),
        Action::Call => "Call".into(),
        Action::Bet(c) => format!("Bet {:.1}bb", bb(*c)),
        Action::Raise(c) => format!("Raise to {:.1}bb", bb(*c)),
        Action::AllIn(c) => format!("All-in {:.1}bb", bb(*c)),
        other => format!("{other:?}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The committed starter manifest must keep resolving to the v1 curated
    /// spots (8 flops, the default srp-btn-bb config), or regenerating would
    /// silently rewrite the library.
    #[test]
    fn starter_manifest_matches_committed_defaults() {
        let spots = load_manifest(&repo_path("manifests/starter-8.toml")).unwrap();
        assert_eq!(spots.len(), 8);
        let expected = SpotConfig::for_formation("srp-btn-bb", repo_path("data/ranges")).unwrap();
        for s in &spots {
            assert_eq!(s.config, expected);
            assert!(s.label.starts_with("SRP BTN vs BB"));
        }
        assert!(spots.iter().any(|s| s.flop == "Td9d6h"));
    }

    #[test]
    fn manifest_overrides_and_unknown_flopsets() {
        let m: Manifest = toml::from_str(
            r#"
            [flopsets]
            tiny = ["Td9d6h"]

            [[runs]]
            formation = "srp-btn-bb"
            flops = "tiny"
            stack_bb = 40.0
            rake_rate = 0.05
            rake_cap_bb = 3.0
            "#,
        )
        .unwrap();
        let spots = manifest_spots(&m).unwrap();
        assert_eq!(spots.len(), 1);
        assert_eq!(spots[0].config.stack_bb, 40.0);
        assert_eq!(spots[0].config.rake_rate, 0.05);
        // The override must land in the cache key.
        let default_ = SpotConfig::for_formation("srp-btn-bb", repo_path("data/ranges")).unwrap();
        assert_ne!(spots[0].config.hash8(), default_.hash8());

        let bad: Manifest =
            toml::from_str("[[runs]]\nformation = \"srp-btn-bb\"\nflops = \"nope\"\n").unwrap();
        assert!(manifest_spots(&bad)
            .unwrap_err()
            .contains("unknown flop set"));
    }

    #[test]
    fn iso_flops_is_the_standard_1755() {
        let flops = iso_flops();
        assert_eq!(flops.len(), 1755);
        // Spot-check: strings parse as flops and are unique.
        assert!(flop_from_str(&flops[0]).is_ok());
        let set: std::collections::BTreeSet<&String> = flops.iter().collect();
        assert_eq!(set.len(), 1755);
    }

    /// Protocol dispatch without a solve: errors are responses, never panics,
    /// and `id` echoes back.
    #[test]
    fn serve_dispatch_handles_errors_id_and_quit() {
        let mut sess = None;

        let (resp, quit) = respond(&mut sess, "not json");
        assert!(resp["error"].as_str().unwrap().contains("bad JSON"));
        assert!(!quit);

        let (resp, _) = respond(&mut sess, r#"{"op":"node","id":7}"#);
        assert!(resp["error"].as_str().unwrap().contains("op:solve first"));
        assert_eq!(resp["id"], 7);

        let (resp, _) = respond(&mut sess, r#"{"op":"warp"}"#);
        assert!(resp["error"].as_str().unwrap().contains("unknown op"));

        // runouts is a known op, so before a solve it fails with "no game".
        let (resp, _) = respond(&mut sess, r#"{"op":"runouts"}"#);
        assert!(resp["error"].as_str().unwrap().contains("op:solve first"));

        // lock/resolve (P10) are known ops too: no game yet, so "solve first".
        for op in ["lock", "resolve"] {
            let (resp, _) = respond(&mut sess, &format!(r#"{{"op":"{op}"}}"#));
            assert!(resp["error"].as_str().unwrap().contains("op:solve first"));
        }

        // v1 (and anything else that isn't v2) is rejected before config parse.
        for v in [1, 9] {
            let (resp, _) = respond(
                &mut sess,
                &format!(r#"{{"v":{v},"op":"solve","config":{{"flop":"Td9d6h"}}}}"#),
            );
            assert!(resp["error"]
                .as_str()
                .unwrap()
                .contains("unsupported protocol"));
        }

        // Right version, malformed config: a protocol error, not a panic.
        let (resp, _) = respond(
            &mut sess,
            r#"{"v":2,"op":"solve","config":{"flop":"Td9d6h"}}"#,
        );
        assert!(resp["error"].as_str().unwrap().contains("bad config"));

        let (resp, quit) = respond(&mut sess, r#"{"op":"quit"}"#);
        assert_eq!(resp["ok"], true);
        assert!(quit);
    }

    #[test]
    fn solve_flags_override_formation_defaults() {
        let cli = Cli::parse_from(["solve-gen", "solve", "--flop", "Td9d6h", "--sizes", "50%"]);
        let Some(Command::Solve(a)) = cli.command else {
            panic!("expected solve subcommand")
        };
        let spot = spot_from_args(&a).unwrap();
        assert_eq!(spot.flop, "Td9d6h");
        assert_eq!(spot.config.flop_sizes, "50%"); // overridden
        assert_eq!(spot.config.turn_sizes, "33%"); // defaulted
        assert_eq!(spot.config.pot_bb, 6.0); // defaulted
        assert!(!spot.config.oop_range.is_empty()); // read from data/ranges
        assert!(spot.label.contains("Td9d6h"));
    }
}
