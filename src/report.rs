//! `report` and `equity` — the P8 aggregate/analysis CLI commands (design 03).
//!
//! Both run entirely off snapshots / range strings, never the solver:
//! - `report` folds the whole snapshot library into one range-averaged row per
//!   (flop, node), with a texture rollup — the Pio-scripting-shaped feature.
//! - `equity` does range-vs-range equity on a flop, plus a per-range histogram.
//!
//! The pure pieces (row summary, aggression test, node label, CSV escaping,
//! histogram binning) are unit-tested; the I/O halves just load and print.

use crate::eval::equity_vs_range;
use crate::solution::{FileSolutionProvider, SolutionProvider, SolvedSpot};
use crate::texture;
use clap::ValueEnum;
use rs_poker::core::Card;
use rs_poker::holdem::RangeParser;
use std::io::Write;
use std::path::Path;

const SOLUTIONS_DIR: &str = "data/solutions";

// ---- report ----------------------------------------------------------------

/// Row sort key for `report --sort`.
#[derive(Clone, Copy, ValueEnum)]
pub enum Sort {
    /// Texture class, then flop (default).
    Texture,
    /// Flop id, alphabetical.
    Flop,
    /// Bet frequency (bet+raise), highest first.
    Bet,
    /// Hero EV, highest first.
    Ev,
}

/// Which side's nodes to keep for `report --node`.
#[derive(Clone, Copy, ValueEnum)]
pub enum NodeSide {
    Ip,
    Oop,
}

/// One report row: a spot's range-averaged summary.
struct Row {
    flop: String,
    texture: &'static str,
    node: String,
    combos: usize,
    /// Bet+raise frequency, range mean, `[0, 1]`.
    bet: f32,
    /// Equilibrium (frequency-weighted) EV in bb, range mean.
    ev: f32,
    /// Range-mean frequency per action, parallel to `actions`.
    mix: Vec<(String, f32)>,
}

/// Is this action label a bet/raise/jam (aggressive), by its first word?
fn is_aggressive(label: &str) -> bool {
    matches!(
        label
            .split_whitespace()
            .next()
            .unwrap_or("")
            .to_ascii_lowercase()
            .as_str(),
        "bet" | "raise" | "jam" | "shove" | "all-in" | "allin"
    )
}

/// The bet size a defending node faces, as a "% pot" pulled from the villain
/// action string ("… bets 2.0bb (33% pot)" → 33); `None` when there's no `%`.
fn facing_pct(villain_action: &str) -> Option<u32> {
    let bytes = villain_action.as_bytes();
    let pct = villain_action.find('%')?;
    let start = bytes[..pct]
        .iter()
        .rposition(|b| !b.is_ascii_digit())
        .map_or(0, |i| i + 1);
    villain_action[start..pct].parse().ok()
}

/// Compact node id: side plus the size a defender faces ("IP", "OOP v33").
fn node_label(spot: &SolvedSpot) -> String {
    let side = if spot.hero_oop { "OOP" } else { "IP" };
    match facing_pct(&spot.villain_action) {
        Some(p) => format!("{side} v{p}"),
        None => side.to_string(),
    }
}

/// First three board cards as a flop, for the texture class.
fn flop_of(board: &[String]) -> Option<[Card; 3]> {
    let card = |s: &String| Card::try_from(s.as_str()).ok();
    match board {
        [a, b, c, ..] => Some([card(a)?, card(b)?, card(c)?]),
        _ => None,
    }
}

/// Fold one spot's per-combo strategies into a range-averaged row. Snapshots
/// carry unit reach weights, so this is the plain per-combo mean — and every
/// combo reaches these first-decision nodes anyway.
fn summarize(spot: &SolvedSpot) -> Option<Row> {
    let actions = &spot.strategies.first()?.strategy.actions;
    let n = spot.strategies.len();
    let mut mix = vec![0.0f32; actions.len()];
    let mut ev = 0.0f32;
    for hs in &spot.strategies {
        let s = &hs.strategy;
        if s.frequencies.len() != actions.len() {
            continue; // a node shares one action set; skip a stray mismatch
        }
        for (acc, f) in mix.iter_mut().zip(&s.frequencies) {
            *acc += f;
        }
        ev += s
            .frequencies
            .iter()
            .zip(&s.action_ev)
            .map(|(f, e)| f * e)
            .sum::<f32>();
    }
    for m in &mut mix {
        *m /= n as f32;
    }
    ev /= n as f32;
    let bet = actions
        .iter()
        .zip(&mix)
        .filter(|(a, _)| is_aggressive(a))
        .map(|(_, f)| *f)
        .sum();
    Some(Row {
        flop: spot.board.join(""),
        texture: flop_of(&spot.board).map_or("?", texture::class),
        node: node_label(spot),
        combos: n,
        bet,
        ev,
        mix: actions.iter().cloned().zip(mix).collect(),
    })
}

/// A spot's formation id; pre-v2 configless files were all `srp-btn-bb`.
fn formation_of(spot: &SolvedSpot) -> &str {
    spot.config.as_ref().map_or("srp-btn-bb", |c| &c.formation)
}

fn sort_rows(rows: &mut [Row], by: Sort) {
    match by {
        Sort::Texture => rows.sort_by(|a, b| a.texture.cmp(b.texture).then(a.flop.cmp(&b.flop))),
        Sort::Flop => rows.sort_by(|a, b| a.flop.cmp(&b.flop)),
        Sort::Bet => rows.sort_by(|a, b| b.bet.total_cmp(&a.bet)),
        Sort::Ev => rows.sort_by(|a, b| b.ev.total_cmp(&a.ev)),
    }
}

/// Mix as one compact string: "Check 55% · Bet 2.0bb 30% · Bet 4.5bb 15%".
fn mix_str(mix: &[(String, f32)]) -> String {
    mix.iter()
        .map(|(a, f)| format!("{a} {:.0}%", f * 100.0))
        .collect::<Vec<_>>()
        .join(" · ")
}

/// Entry point for `poker-trainer report`.
pub fn run_report(
    formation: Option<String>,
    node: Option<NodeSide>,
    sort: Sort,
    csv: Option<&Path>,
) {
    let provider = match FileSolutionProvider::load(SOLUTIONS_DIR) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("Couldn't load {SOLUTIONS_DIR} ({e}) — run `cargo run -p solve-gen` first.");
            return;
        }
    };
    let mut rows: Vec<Row> = provider
        .spots()
        .iter()
        .filter(|s| formation.as_deref().is_none_or(|f| formation_of(s) == f))
        .filter(|s| node.is_none_or(|n| s.hero_oop == matches!(n, NodeSide::Oop)))
        .filter_map(summarize)
        .collect();
    if rows.is_empty() {
        eprintln!("No matching spots in {SOLUTIONS_DIR}.");
        return;
    }
    sort_rows(&mut rows, sort);

    if let Some(path) = csv {
        match write_csv(&rows, path) {
            Ok(()) => println!("Wrote {} rows to {}", rows.len(), path.display()),
            Err(e) => eprintln!("Couldn't write {}: {e}", path.display()),
        }
        return;
    }
    print_table(&rows);
    print_rollup(&rows);
}

fn print_table(rows: &[Row]) {
    println!(
        "{:<8} {:<9} {:<7} {:>6} {:>6} {:>7}  mix",
        "flop", "texture", "node", "combos", "bet%", "ev(bb)"
    );
    for r in rows {
        println!(
            "{:<8} {:<9} {:<7} {:>6} {:>5.0}% {:>+7.2}  {}",
            r.flop,
            r.texture,
            r.node,
            r.combos,
            r.bet * 100.0,
            r.ev,
            mix_str(&r.mix),
        );
    }
}

/// Per-texture rollup: count, mean bet% ± spread, mean EV ± spread.
fn print_rollup(rows: &[Row]) {
    let mut classes: Vec<&str> = rows.iter().map(|r| r.texture).collect();
    classes.sort_unstable();
    classes.dedup();
    println!(
        "\n{:<9} {:>6} {:>14} {:>16}",
        "texture", "rows", "bet% (mean±sd)", "ev(bb) (mean±sd)"
    );
    for tex in classes {
        let grp: Vec<&Row> = rows.iter().filter(|r| r.texture == tex).collect();
        let (bm, bs) = mean_sd(grp.iter().map(|r| r.bet * 100.0));
        let (em, es) = mean_sd(grp.iter().map(|r| r.ev));
        println!(
            "{:<9} {:>6} {:>8.0} ±{:>4.0} {:>10.2} ±{:>4.2}",
            tex,
            grp.len(),
            bm,
            bs,
            em,
            es
        );
    }
}

/// Mean and population standard deviation of a sample (0, 0 if empty).
fn mean_sd(xs: impl Iterator<Item = f32>) -> (f32, f32) {
    let v: Vec<f32> = xs.collect();
    if v.is_empty() {
        return (0.0, 0.0);
    }
    let mean = v.iter().sum::<f32>() / v.len() as f32;
    let var = v.iter().map(|x| (x - mean).powi(2)).sum::<f32>() / v.len() as f32;
    (mean, var.sqrt())
}

/// Quote a CSV field iff it holds a comma, quote, or newline (RFC-4180-ish).
fn csv_field(s: &str) -> String {
    if s.contains([',', '"', '\n']) {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

fn write_csv(rows: &[Row], path: &Path) -> std::io::Result<()> {
    let mut out = String::from("flop,texture,node,combos,bet_pct,ev_bb,mix\n");
    for r in rows {
        out.push_str(&format!(
            "{},{},{},{},{:.1},{:.3},{}\n",
            csv_field(&r.flop),
            r.texture,
            csv_field(&r.node),
            r.combos,
            r.bet * 100.0,
            r.ev,
            csv_field(&mix_str(&r.mix)),
        ));
    }
    std::fs::write(path, out)
}

// ---- equity -----------------------------------------------------------------

/// Monte-Carlo runouts per (hero, villain) pair, and the cap on how many
/// opposing combos each hero is measured against — the range mean averages out
/// the variance across combos, so both can stay small.
// ponytail: O(hero × villain) Monte Carlo. Exact turn+river enumeration (990
// runouts on a flop) is the upgrade path if a tighter number is ever needed.
const EQ_ITERS: u32 = 60;
const OPP_CAP: usize = 120;

/// Parse a range string into concrete two-card combos.
fn parse_range(s: &str) -> Result<Vec<[Card; 2]>, String> {
    RangeParser::parse_many(s)
        .map_err(|e| format!("bad range {s:?}: {e}"))
        .map(|hands| hands.iter().map(|h| [h[0], h[1]]).collect())
}

/// Parse a packed board like "Td9d6h" into exactly three cards.
fn parse_flop(board: &str) -> Option<[Card; 3]> {
    let cards: Option<Vec<Card>> = board
        .as_bytes()
        .chunks(2)
        .map(|c| {
            std::str::from_utf8(c)
                .ok()
                .and_then(|s| Card::try_from(s).ok())
        })
        .collect();
    cards?.try_into().ok()
}

/// Drop combos that collide with the flop (dead cards).
fn live_combos(range: Vec<[Card; 2]>, flop: [Card; 3]) -> Vec<[Card; 2]> {
    range
        .into_iter()
        .filter(|c| !c.iter().any(|card| flop.contains(card)))
        .collect()
}

/// Per-combo equity of each hand in `hero` vs the `villain` range on `flop`.
/// The villain range is sampled to `OPP_CAP` for speed (a per-hero fixed prefix
/// is fine — the histogram/mean is over hero combos, not villain).
fn combo_equities(hero: &[[Card; 2]], villain: &[[Card; 2]], flop: [Card; 3]) -> Vec<f64> {
    let sample = &villain[..villain.len().min(OPP_CAP)];
    hero.iter()
        .map(|&h| equity_vs_range(h, flop, sample, EQ_ITERS))
        .collect()
}

/// A 10-bin `[0,100%)` histogram of equities, as counts.
fn histogram(eqs: &[f64]) -> [usize; 10] {
    let mut bins = [0usize; 10];
    for &e in eqs {
        let b = ((e * 10.0) as usize).min(9);
        bins[b] += 1;
    }
    bins
}

fn print_histogram(label: &str, eqs: &[f64]) {
    let mean = if eqs.is_empty() {
        0.0
    } else {
        eqs.iter().sum::<f64>() / eqs.len() as f64
    };
    println!(
        "\n{label} equity distribution (n={} combos, mean {:.1}%):",
        eqs.len(),
        mean * 100.0
    );
    let bins = histogram(eqs);
    let peak = bins.iter().copied().max().unwrap_or(0).max(1);
    for (i, &count) in bins.iter().enumerate() {
        let bar = "█".repeat(count * 40 / peak);
        println!("  {:>2}-{:>3}% | {bar} {count}", i * 10, i * 10 + 10);
    }
}

/// Entry point for `poker-trainer equity`.
pub fn run_equity(oop: &str, ip: &str, board: &str) {
    let Some(flop) = parse_flop(board) else {
        eprintln!("--board needs 3 cards on the flop, e.g. Td9d6h.");
        return;
    };
    let (oop, ip) = match (parse_range(oop), parse_range(ip)) {
        (Ok(o), Ok(i)) => (live_combos(o, flop), live_combos(i, flop)),
        (Err(e), _) | (_, Err(e)) => {
            eprintln!("{e}");
            return;
        }
    };
    if oop.is_empty() || ip.is_empty() {
        eprintln!("A range is empty after removing board cards — nothing to match up.");
        return;
    }

    eprint!(
        "Computing range-vs-range equity ({} × {} combos)… ",
        oop.len(),
        ip.len()
    );
    let _ = std::io::stderr().flush();
    let oop_eqs = combo_equities(&oop, &ip, flop);
    let ip_eqs = combo_equities(&ip, &oop, flop);
    eprintln!("done.");

    // Headline number from the OOP mean; heads-up with split ties, IP is the
    // exact complement, so report it that way rather than the noisier IP pass.
    let oop_mean = oop_eqs.iter().sum::<f64>() / oop_eqs.len() as f64;
    println!(
        "Board {}  —  OOP {:.1}%  vs  IP {:.1}%",
        board,
        oop_mean * 100.0,
        (1.0 - oop_mean) * 100.0
    );
    print_histogram("OOP", &oop_eqs);
    print_histogram("IP", &ip_eqs);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::solution::{HandStrategy, NodeStrategy};

    fn spot(hero_oop: bool, villain_action: &str, strategies: Vec<HandStrategy>) -> SolvedSpot {
        SolvedSpot {
            label: "t".into(),
            board: vec!["Td".into(), "9d".into(), "6h".into()],
            pot_bb: 6.0,
            hero_oop,
            villain_action: villain_action.into(),
            config: None,
            generator: None,
            strategies,
        }
    }

    fn hs(hand: &str, freqs: Vec<f32>, evs: Vec<f32>, actions: &[&str]) -> HandStrategy {
        HandStrategy {
            hand: hand.into(),
            strategy: NodeStrategy {
                actions: actions.iter().map(|s| s.to_string()).collect(),
                frequencies: freqs,
                action_ev: evs,
            },
        }
    }

    #[test]
    fn aggression_is_by_first_word() {
        assert!(is_aggressive("Bet 2.0bb"));
        assert!(is_aggressive("Raise to 4.9bb"));
        assert!(!is_aggressive("Check"));
        assert!(!is_aggressive("Call"));
        assert!(!is_aggressive("Fold"));
    }

    #[test]
    fn facing_pct_pulls_the_size_or_none() {
        assert_eq!(
            facing_pct("You check, villain bets 4.5bb (75% pot)"),
            Some(75)
        );
        assert_eq!(facing_pct("Villain (BB) checks to you"), None);
        assert_eq!(node_label(&spot(false, "Villain checks", vec![])), "IP");
        assert_eq!(
            node_label(&spot(true, "villain bets (33% pot)", vec![])),
            "OOP v33"
        );
    }

    #[test]
    fn summarize_means_freqs_and_equilibrium_ev() {
        let actions = ["Check", "Bet 2.0bb"];
        // c1: 100% check, EV 1.0 played. c2: 50/50, EV .5*2 + .5*4 = 3.0.
        let s = spot(
            false,
            "checks",
            vec![
                hs("AsKs", vec![1.0, 0.0], vec![1.0, 0.5], &actions),
                hs("AhKh", vec![0.5, 0.5], vec![2.0, 4.0], &actions),
            ],
        );
        let row = summarize(&s).unwrap();
        assert_eq!(row.combos, 2);
        assert_eq!(row.node, "IP");
        assert_eq!(row.texture, "two-tone");
        // mean bet freq = (0.0 + 0.5) / 2 = 0.25
        assert!((row.bet - 0.25).abs() < 1e-6);
        // equilibrium EV per combo: c1 = 1.0, c2 = 3.0; mean = 2.0
        assert!((row.ev - 2.0).abs() < 1e-6);
    }

    #[test]
    fn sort_by_bet_is_descending() {
        let mk = |flop: &str, bet: f32| Row {
            flop: flop.into(),
            texture: "x",
            node: "IP".into(),
            combos: 1,
            bet,
            ev: 0.0,
            mix: vec![],
        };
        let mut rows = vec![mk("a", 0.2), mk("b", 0.9), mk("c", 0.5)];
        sort_rows(&mut rows, Sort::Bet);
        assert_eq!(
            rows.iter().map(|r| r.flop.as_str()).collect::<Vec<_>>(),
            ["b", "c", "a"]
        );
    }

    #[test]
    fn csv_field_quotes_only_when_needed() {
        assert_eq!(csv_field("Check"), "Check");
        assert_eq!(csv_field("a · b"), "a · b");
        assert_eq!(csv_field("Check 55%, Bet 45%"), "\"Check 55%, Bet 45%\"");
        assert_eq!(csv_field("say \"hi\""), "\"say \"\"hi\"\"\"");
    }

    #[test]
    fn mean_sd_is_population_stat() {
        let (m, sd) = mean_sd([2.0, 4.0].into_iter());
        assert!((m - 3.0).abs() < 1e-6);
        assert!((sd - 1.0).abs() < 1e-6); // population sd of {2,4}
        assert_eq!(mean_sd(std::iter::empty()), (0.0, 0.0));
    }

    #[test]
    fn parse_range_expands_to_combos() {
        assert_eq!(parse_range("AA").unwrap().len(), 6); // six combos of aces
        assert_eq!(parse_range("AKs").unwrap().len(), 4); // four suited
        assert!(parse_range("ZZ").is_err());
    }

    #[test]
    fn live_combos_drops_board_collisions() {
        let flop = parse_flop("Td9d6h").unwrap();
        // AA has 6 combos, none touch the board -> all live.
        assert_eq!(live_combos(parse_range("AA").unwrap(), flop).len(), 6);
        // TT: the Td is on the board, so combos holding Td are dead (3 of 6).
        assert_eq!(live_combos(parse_range("TT").unwrap(), flop).len(), 3);
    }

    #[test]
    fn parse_flop_needs_three_cards() {
        assert!(parse_flop("Td9d6h").is_some());
        assert!(parse_flop("Td9d").is_none());
        assert!(parse_flop("Td9d6h2c").is_none());
        assert!(parse_flop("zz9d6h").is_none());
    }

    #[test]
    fn histogram_bins_by_tenths() {
        let bins = histogram(&[0.0, 0.05, 0.5, 0.99, 1.0]);
        assert_eq!(bins[0], 2); // 0.00, 0.05
        assert_eq!(bins[5], 1); // 0.50
        assert_eq!(bins[9], 2); // 0.99 and 1.0 (clamped into the last bin)
    }
}
