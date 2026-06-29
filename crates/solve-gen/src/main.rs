//! Offline GTO solution generator (AGPL — links postflop-solver).
//!
//! For each curated spot, solve a single-raised pot to equilibrium, navigate to
//! the hero's decision (OOP checks, IP c-bets, hero faces the bet), and dump the
//! per-hand action mix + per-action EV as `data/solutions/<board>.json`. The
//! trainer reads those files and never links this crate.

use poker_trainer::solution::{HandStrategy, NodeStrategy, SolvedSpot};
use postflop_solver::*;
use std::fs;
use std::path::Path;

const CHIPS_PER_BB: f32 = 100.0;

/// One spot to solve. Hero is always OOP facing a flop c-bet here (v1).
struct Spot {
    label: &'static str,
    flop: &'static str,
    oop_range: &'static str,
    ip_range: &'static str,
}

fn main() {
    // Wide-ish SRP BTN-vs-BB ranges. Hero = BB (OOP).
    const OOP: &str =
        "22+,A2s+,K2s+,Q5s+,J7s+,T7s+,96s+,86s+,75s+,64s+,53s+,A2o+,K9o+,Q9o+,J9o+,T9o,98o";
    const IP: &str =
        "22+,A2s+,K2s+,Q4s+,J6s+,T6s+,96s+,85s+,75s+,64s+,53s+,43s,A2o+,K7o+,Q8o+,J8o+,T8o+,98o";

    // Base label = position + board + texture tag; each solve appends the
    // node-specific question (c-bet? / defend?). Aim for texture spread.
    let spots = [
        Spot {
            label: "SRP BTN vs BB, Td9d6h (wet)",
            flop: "Td9d6h",
            oop_range: OOP,
            ip_range: IP,
        },
        Spot {
            label: "SRP BTN vs BB, Kh7c2d (dry)",
            flop: "Kh7c2d",
            oop_range: OOP,
            ip_range: IP,
        },
        Spot {
            label: "SRP BTN vs BB, Ah8h3h (monotone)",
            flop: "Ah8h3h",
            oop_range: OOP,
            ip_range: IP,
        },
        Spot {
            label: "SRP BTN vs BB, 8h8c3d (paired)",
            flop: "8h8c3d",
            oop_range: OOP,
            ip_range: IP,
        },
        Spot {
            label: "SRP BTN vs BB, QhJd9c (broadway)",
            flop: "QhJd9c",
            oop_range: OOP,
            ip_range: IP,
        },
        Spot {
            label: "SRP BTN vs BB, As7d2c (ace-high dry)",
            flop: "As7d2c",
            oop_range: OOP,
            ip_range: IP,
        },
        Spot {
            label: "SRP BTN vs BB, 6h5d4c (low connected)",
            flop: "6h5d4c",
            oop_range: OOP,
            ip_range: IP,
        },
        Spot {
            label: "SRP BTN vs BB, 9s8s4d (two-tone mid)",
            flop: "9s8s4d",
            oop_range: OOP,
            ip_range: IP,
        },
    ];

    let out_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/solutions");
    fs::create_dir_all(&out_dir).unwrap();

    for spot in &spots {
        println!("Solving: {}", spot.label);
        // One solved game yields two nodes (IP c-bet decision, OOP defend).
        for solved in solve_spot(spot) {
            let role = if solved.hero_oop { "oop" } else { "ip" };
            let file = out_dir.join(format!("{}-{}.json", spot.flop.to_lowercase(), role));
            fs::write(&file, serde_json::to_string_pretty(&solved).unwrap()).unwrap();
            println!(
                "  -> {} ({} hero hands)",
                file.display(),
                solved.strategies.len()
            );
        }
    }
}

fn solve_spot(spot: &Spot) -> Vec<SolvedSpot> {
    let starting_pot = (6.0 * CHIPS_PER_BB) as i32; // 6bb SRP pot
    let card_config = CardConfig {
        range: [
            spot.oop_range.parse().unwrap(),
            spot.ip_range.parse().unwrap(),
        ],
        flop: flop_from_str(spot.flop).unwrap(),
        turn: NOT_DEALT,
        river: NOT_DEALT,
    };
    // Two flop c-bet sizes so the c-bet node is a real size-mix decision.
    // ponytail: turn/river stay single-size to bound tree growth (one size was
    // applied to every street before) — widen them too if you train later nodes.
    let flop_bets = BetSizeOptions::try_from(("33%, 75%", "2.5x")).unwrap();
    let later_bets = BetSizeOptions::try_from(("33%", "2.5x")).unwrap();
    let tree_config = TreeConfig {
        initial_state: BoardState::Flop,
        starting_pot,
        effective_stack: (97.0 * CHIPS_PER_BB) as i32,
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
    let board: Vec<String> = flop_from_str(spot.flop)
        .unwrap()
        .iter()
        .map(|&c| card_to_string(c).unwrap())
        .collect();

    // Both decision nodes come from this one solved game: OOP checks, then IP
    // decides whether to c-bet (hero = BTN), then OOP faces the bet (hero = BB).
    let mut out = Vec::with_capacity(2);
    game.back_to_root();
    assert_eq!(game.current_player(), 0, "root should be OOP");
    game.play(action_index(&game, |a| matches!(a, Action::Check)));

    // Node 1: hero is IP (BTN), villain (BB) has checked — c-bet or check back?
    assert_eq!(
        game.current_player(),
        1,
        "after check, IP (hero) decides whether to c-bet"
    );
    let bet_actions = game
        .available_actions()
        .iter()
        .filter(|a| matches!(a, Action::Bet(_)))
        .count();
    assert!(
        bet_actions >= 2,
        "c-bet node should offer >=2 sizes, got {bet_actions} (bet-size config didn't widen?)"
    );
    out.push(extract(
        &mut game,
        format!("{} — you're BTN, BB checks: c-bet?", spot.label),
        board.clone(),
        pot_bb,
        false,
        "Villain (BB) checks to you".to_string(),
    ));

    // Descend into the c-bet, then Node 2: hero is OOP (BB) facing the bet.
    let ip_bet = action_index(&game, |a| matches!(a, Action::Bet(_)));
    let bet_chips = match game.available_actions()[ip_bet] {
        Action::Bet(c) => c,
        _ => unreachable!(),
    };
    game.play(ip_bet);
    assert_eq!(game.current_player(), 0, "hero (OOP) faces the bet");
    let bet_bb = bet_chips as f32 / CHIPS_PER_BB;
    out.push(extract(
        &mut game,
        format!("{} — you're BB, facing BTN c-bet: defend?", spot.label),
        board,
        pot_bb,
        true,
        format!(
            "You check, villain bets {bet_bb:.1}bb ({:.0}% pot)",
            100.0 * bet_chips as f32 / starting_pot as f32
        ),
    ));

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
