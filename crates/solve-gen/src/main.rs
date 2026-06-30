//! Offline GTO solution generator (AGPL — links postflop-solver).
//!
//! For each curated spot, solve a single-raised pot to equilibrium, navigate to
//! the hero's decision (OOP checks, IP c-bets, hero faces the bet), and dump the
//! per-hand action mix + per-action EV as `data/solutions/<board>.json`. The
//! trainer reads those files and never links this crate.

use clap::{Args, Parser, Subcommand};
use poker_trainer::solution::{HandStrategy, NodeStrategy, SolvedSpot};
use postflop_solver::*;
use std::fs;
use std::path::{Path, PathBuf};

const CHIPS_PER_BB: f32 = 100.0;

// Wide-ish SRP BTN-vs-BB ranges; the defaults for a custom solve.
const OOP: &str =
    "22+,A2s+,K2s+,Q5s+,J7s+,T7s+,96s+,86s+,75s+,64s+,53s+,A2o+,K9o+,Q9o+,J9o+,T9o,98o"; // hero = BB
const IP: &str =
    "22+,A2s+,K2s+,Q4s+,J6s+,T6s+,96s+,85s+,75s+,64s+,53s+,43s,A2o+,K7o+,Q8o+,J8o+,T8o+,98o";
const DEFAULT_SIZES: &str = "33%, 75%"; // flop c-bet sizes
const DEFAULT_STACK_BB: f32 = 97.0;
const DEFAULT_POT_BB: f32 = 6.0;

/// One spot to solve: a BTN-vs-BB SRP whose flop, ranges, and game knobs are
/// configurable. One solve yields the BTN c-bet node + one BB defend node per
/// c-bet size.
struct Spot {
    label: String,
    flop: String,
    oop_range: String,
    ip_range: String,
    /// Flop c-bet sizes, e.g. `"33%, 75%"` (parsed by postflop-solver).
    flop_bets: String,
    stack_bb: f32,
    pot_bb: f32,
}

#[derive(Parser)]
#[command(name = "solve-gen", about = "Offline GTO solution generator")]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Regenerate the curated solution library (default).
    Gen,
    /// Solve one custom spot and write its JSON into the solution dir.
    Solve(SolveArgs),
}

#[derive(Args)]
struct SolveArgs {
    /// Flop as rs_poker cards, e.g. `Td9d6h`.
    #[arg(long)]
    flop: String,
    /// OOP (BB) range string.
    #[arg(long, default_value = OOP)]
    oop: String,
    /// IP (BTN) range string.
    #[arg(long, default_value = IP)]
    ip: String,
    /// Flop c-bet sizes, e.g. `"33%, 75%"`.
    #[arg(long, default_value = DEFAULT_SIZES)]
    sizes: String,
    /// Effective stack in bb.
    #[arg(long, default_value_t = DEFAULT_STACK_BB)]
    stack: f32,
    /// Starting pot in bb.
    #[arg(long, default_value_t = DEFAULT_POT_BB)]
    pot: f32,
    /// Output directory (defaults to the repo's data/solutions).
    #[arg(long)]
    out: Option<PathBuf>,
}

fn main() {
    let default_out = || Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/solutions");
    match Cli::parse().command.unwrap_or(Command::Gen) {
        Command::Gen => write_all(&curated(), &default_out()),
        Command::Solve(a) => {
            let out = a.out.clone().unwrap_or_else(default_out);
            write_all(std::slice::from_ref(&spot_from_args(a)), &out);
        }
    }
}

fn spot_from_args(a: SolveArgs) -> Spot {
    Spot {
        label: format!("Custom BTN vs BB, {}", a.flop),
        flop: a.flop,
        oop_range: a.oop,
        ip_range: a.ip,
        flop_bets: a.sizes,
        stack_bb: a.stack,
        pot_bb: a.pot,
    }
}

/// The curated, texture-spread library. Defaults match the v1 hardcoded config
/// so regenerating produces byte-identical files.
fn curated() -> Vec<Spot> {
    [
        ("SRP BTN vs BB, Td9d6h (wet)", "Td9d6h"),
        ("SRP BTN vs BB, Kh7c2d (dry)", "Kh7c2d"),
        ("SRP BTN vs BB, Ah8h3h (monotone)", "Ah8h3h"),
        ("SRP BTN vs BB, 8h8c3d (paired)", "8h8c3d"),
        ("SRP BTN vs BB, QhJd9c (broadway)", "QhJd9c"),
        ("SRP BTN vs BB, As7d2c (ace-high dry)", "As7d2c"),
        ("SRP BTN vs BB, 6h5d4c (low connected)", "6h5d4c"),
        ("SRP BTN vs BB, 9s8s4d (two-tone mid)", "9s8s4d"),
    ]
    .into_iter()
    .map(|(label, flop)| Spot {
        label: label.into(),
        flop: flop.into(),
        oop_range: OOP.into(),
        ip_range: IP.into(),
        flop_bets: DEFAULT_SIZES.into(),
        stack_bb: DEFAULT_STACK_BB,
        pot_bb: DEFAULT_POT_BB,
    })
    .collect()
}

fn write_all(spots: &[Spot], out_dir: &Path) {
    fs::create_dir_all(out_dir).unwrap();
    for spot in spots {
        println!("Solving: {}", spot.label);
        // One solved game yields the IP c-bet node plus one OOP defend node per
        // c-bet size; solve_spot hands back each with its own file stem.
        for (stem, solved) in solve_spot(spot) {
            let file = out_dir.join(format!("{stem}.json"));
            fs::write(&file, serde_json::to_string_pretty(&solved).unwrap()).unwrap();
            println!(
                "  -> {} ({} hero hands)",
                file.display(),
                solved.strategies.len()
            );
        }
    }
}

fn solve_spot(spot: &Spot) -> Vec<(String, SolvedSpot)> {
    let starting_pot = (spot.pot_bb * CHIPS_PER_BB) as i32;
    let card_config = CardConfig {
        range: [
            spot.oop_range.parse().unwrap(),
            spot.ip_range.parse().unwrap(),
        ],
        flop: flop_from_str(&spot.flop).unwrap(),
        turn: NOT_DEALT,
        river: NOT_DEALT,
    };
    // Configurable flop c-bet sizes so the c-bet node is a real size-mix.
    // ponytail: turn/river stay single-size to bound tree growth (one size was
    // applied to every street before) — widen them too if you train later nodes.
    let flop_bets = BetSizeOptions::try_from((spot.flop_bets.as_str(), "2.5x")).unwrap();
    let later_bets = BetSizeOptions::try_from(("33%", "2.5x")).unwrap();
    let tree_config = TreeConfig {
        initial_state: BoardState::Flop,
        starting_pot,
        effective_stack: (spot.stack_bb * CHIPS_PER_BB) as i32,
        rake_rate: 0.0,
        rake_cap: 0.0,
        flop_bet_sizes: [flop_bets.clone(), flop_bets.clone()],
        turn_bet_sizes: [later_bets.clone(), later_bets.clone()],
        river_bet_sizes: [later_bets.clone(), later_bets.clone()],
        turn_donk_sizes: None,
        river_donk_sizes: None,
        add_allin_threshold: 1.5,
        force_allin_threshold: 0.15,
        merging_threshold: 0.1,
    };

    let action_tree = ActionTree::new(tree_config).unwrap();
    let mut game = PostFlopGame::with_config(card_config, action_tree).unwrap();
    game.allocate_memory(false);
    let target = starting_pot as f32 * 0.005; // 0.5% of pot
    let exploitability = solve(&mut game, 1000, target, false);
    println!(
        "  exploitability: {:.3} chips ({:.3}bb)",
        exploitability,
        exploitability / CHIPS_PER_BB
    );

    let pot_bb = starting_pot as f32 / CHIPS_PER_BB;
    let board: Vec<String> = flop_from_str(&spot.flop)
        .unwrap()
        .iter()
        .map(|&c| card_to_string(c).unwrap())
        .collect();

    // All decision nodes come from this one solved game. Navigate: OOP checks,
    // IP decides whether to c-bet (hero = BTN), then OOP faces each c-bet size
    // (hero = BB) — one defend node per size for a symmetric library.
    let stem = spot.flop.to_lowercase();
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

    // Node 1: hero is IP (BTN), villain (BB) has checked — c-bet or check back?
    to_cbet(&mut game);
    let bet_indices: Vec<usize> = game
        .available_actions()
        .iter()
        .enumerate()
        .filter(|(_, a)| matches!(a, Action::Bet(_)))
        .map(|(i, _)| i)
        .collect();
    assert!(
        bet_indices.len() >= 2,
        "c-bet node should offer >=2 sizes, got {} (bet-size config didn't widen?)",
        bet_indices.len()
    );
    out.push((
        format!("{stem}-ip"),
        extract(
            &mut game,
            format!("{} — you're BTN, BB checks: c-bet?", spot.label),
            board.clone(),
            pot_bb,
            false,
            "Villain (BB) checks to you".to_string(),
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
                    "{} — you're BB, facing BTN {pct}% c-bet: defend?",
                    spot.label
                ),
                board.clone(),
                pot_bb,
                true,
                format!("You check, villain bets {bet_bb:.1}bb ({pct}% pot)"),
            ),
        ));
        to_cbet(&mut game); // reset to the c-bet node for the next size
    }

    out
}

/// Build a [`SolvedSpot`] from the game positioned at the hero's decision node.
/// Node-specific bits (`label`, `hero_oop`, `villain_action`) are passed in; the
/// strategy/EV read off `current_player()` is the same for any node.
fn extract(
    game: &mut PostFlopGame,
    label: String,
    board: Vec<String>,
    pot_bb: f32,
    hero_oop: bool,
    villain_action: String,
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

    /// The curated library must keep solving with the v1 game config, or
    /// regenerating would silently rewrite the committed JSON.
    #[test]
    fn curated_uses_the_committed_defaults() {
        let spots = curated();
        assert_eq!(spots.len(), 8);
        for s in &spots {
            assert_eq!(s.oop_range, OOP);
            assert_eq!(s.ip_range, IP);
            assert_eq!(s.flop_bets, "33%, 75%");
            assert_eq!(s.stack_bb, 97.0);
            assert_eq!(s.pot_bb, 6.0);
        }
    }

    #[test]
    fn solve_flag_maps_to_spot_with_defaults_for_the_rest() {
        let cli = Cli::parse_from(["solve-gen", "solve", "--flop", "Td9d6h", "--sizes", "50%"]);
        let Some(Command::Solve(a)) = cli.command else {
            panic!("expected solve subcommand")
        };
        let spot = spot_from_args(a);
        assert_eq!(spot.flop, "Td9d6h");
        assert_eq!(spot.flop_bets, "50%"); // overridden
        assert_eq!(spot.oop_range, OOP); // defaulted
        assert_eq!(spot.pot_bb, 6.0); // defaulted
        assert!(spot.label.contains("Td9d6h"));
    }
}
