//! The preflop chart format: what `crates/preflop-gen` (the permissive CFR
//! generator) writes under `data/preflop/<ruleset>/` and what the trainer and
//! the web browser read. Sibling seam to [`crate::solution`] — the format
//! lives here, in the trainer, so the generator depends on us and not the
//! other way around (design doc 07).
//!
//! A ruleset directory holds `header.json` (config echo + provenance) and
//! `starter.jsonl` / `charts.jsonl` (one [`PreflopNode`] per line). Nodes are
//! addressed by their action path: tokens `f | c | r<to-bb> | ai` joined by
//! `-`, e.g. `f-f-r2.5-f-c` = folded to CO, CO opens 2.5bb, BTN folds, SB
//! calls — BB to act. The root (first seat to act, nobody in yet) is `""`.

use crate::solution::NodeStrategy;
use rand::RngExt;
use rs_poker::core::Card;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::Path;

/// Ranks in chart order: the row/column order of every 13×13 grid.
pub const RANKS: [char; 13] = [
    'A', 'K', 'Q', 'J', 'T', '9', '8', '7', '6', '5', '4', '3', '2',
];

/// Number of canonical preflop hand classes (13 pairs + 78 suited + 78 offsuit).
pub const CLASSES: usize = 169;

/// Name of hand class `i` in row-major 13×13 grid order: diagonal = pairs,
/// upper triangle = suited, lower = offsuit. `0 = "AA"`, `1 = "AKs"`,
/// `13 = "AKo"`, `168 = "22"`.
pub fn class_name(i: usize) -> String {
    let (r, c) = (i / 13, i % 13);
    match r.cmp(&c) {
        std::cmp::Ordering::Equal => format!("{}{}", RANKS[r], RANKS[c]),
        std::cmp::Ordering::Less => format!("{}{}s", RANKS[r], RANKS[c]),
        std::cmp::Ordering::Greater => format!("{}{}o", RANKS[c], RANKS[r]),
    }
}

/// Grid index of a class name (`"AKs"` → 1). Inverse of [`class_name`].
pub fn class_index_of(name: &str) -> Option<usize> {
    let mut ch = name.chars();
    let (a, b, suffix) = (ch.next()?, ch.next()?, ch.next());
    let rank = |c| RANKS.iter().position(|&r| r == c);
    let (hi, lo) = (rank(a)?, rank(b)?);
    match (suffix, ch.next()) {
        (None, None) if hi == lo => Some(hi * 13 + lo),
        (Some('s'), None) if hi < lo => Some(hi * 13 + lo),
        (Some('o'), None) if hi < lo => Some(lo * 13 + hi),
        _ => None,
    }
}

/// Grid index of a concrete two-card holding.
pub fn class_index(hand: [Card; 2]) -> usize {
    let rank = |card: &Card| {
        let c = char::from(card.value);
        RANKS.iter().position(|&r| r == c).expect("rank char")
    };
    let (a, b) = (rank(&hand[0]), rank(&hand[1]));
    let (hi, lo) = (a.min(b), a.max(b));
    if hand[0].suit == hand[1].suit {
        hi * 13 + lo // suited (a pair can't be: same rank+suit is one card)
    } else {
        lo * 13 + hi // offsuit; for pairs hi == lo so both formulas agree
    }
}

/// Parse a card string like `"AhKh"` or `"Td 9d 6h"` into its cards (any
/// count). `None` if the whitespace-stripped length is odd/zero or any two-char
/// code is invalid. Doesn't check for duplicates — callers that care do.
pub fn parse_cards(text: &str) -> Option<Vec<Card>> {
    let compact: String = text.chars().filter(|c| !c.is_whitespace()).collect();
    if compact.is_empty() || !compact.len().is_multiple_of(2) {
        return None;
    }
    (0..compact.len())
        .step_by(2)
        .map(|i| Card::try_from(&compact[i..i + 2]).ok())
        .collect()
}

/// How many concrete combos class `i` has: 6 per pair, 4 suited, 12 offsuit.
pub fn class_combos(i: usize) -> u32 {
    let (r, c) = (i / 13, i % 13);
    match r.cmp(&c) {
        std::cmp::Ordering::Equal => 6,
        std::cmp::Ordering::Less => 4,
        std::cmp::Ordering::Greater => 12,
    }
}

/// The concrete combos of class `i`, the list form of [`class_combos`]:
/// 6 for a pair, 4 suited, 12 offsuit. Inverse of [`class_index`].
pub fn class_combos_cards(i: usize) -> Vec<[Card; 2]> {
    const SUITS: [char; 4] = ['s', 'h', 'd', 'c'];
    let card = |rank: usize, suit: char| {
        Card::try_from(format!("{}{suit}", RANKS[rank]).as_str()).expect("valid card code")
    };
    let (r, c) = (i / 13, i % 13);
    let mut combos = Vec::new();
    match r.cmp(&c) {
        // pair: the 6 unordered suit pairs
        std::cmp::Ordering::Equal => {
            for (a, &s1) in SUITS.iter().enumerate() {
                for &s2 in &SUITS[a + 1..] {
                    combos.push([card(r, s1), card(r, s2)]);
                }
            }
        }
        // suited: higher rank `r`, lower rank `c`, matching suits
        std::cmp::Ordering::Less => {
            for &s in &SUITS {
                combos.push([card(r, s), card(c, s)]);
            }
        }
        // offsuit: higher rank `c`, lower rank `r`, mismatched suits
        std::cmp::Ordering::Greater => {
            for &s1 in &SUITS {
                for &s2 in &SUITS {
                    if s1 != s2 {
                        combos.push([card(c, s1), card(r, s2)]);
                    }
                }
            }
        }
    }
    combos
}

/// Hero's equity against a villain *range* given the villain seat's per-class
/// `reach` (as produced by [`PreflopCharts::class_reach`]): sample up to `cap`
/// villain combos with per-combo weight `reach[class]` — dropping combos that
/// collide with the hero or `board` (3–5 cards) — and average equity over the
/// sample. Returns `0.5` when the range is empty after card removal.
///
/// Sampling proportional to reach lets the unweighted [`crate::eval::equity_vs_range`]
/// stand in for a reach-weighted mean, so there's no separate weighted path.
///
/// ponytail: reach is class-level (blocker effects ignored, like `class_reach`)
/// and the estimate is sampled Monte Carlo — raise `cap`/`iters` if a decision
/// boundary needs a tighter read.
pub fn equity_vs_reach(
    hero: [Card; 2],
    board: &[Card],
    reach: &[f32],
    rng: &mut impl RngExt,
    iters: u32,
    cap: usize,
) -> f64 {
    let dead = |v: &[Card; 2]| v.iter().any(|c| hero.contains(c) || board.contains(c));
    let mut combos: Vec<[Card; 2]> = Vec::new();
    let mut weights: Vec<f32> = Vec::new();
    for (i, &w) in reach.iter().take(CLASSES).enumerate() {
        if w <= 0.0 {
            continue;
        }
        for combo in class_combos_cards(i) {
            if !dead(&combo) {
                combos.push(combo);
                weights.push(w);
            }
        }
    }
    let total: f32 = weights.iter().sum();
    if combos.is_empty() || total <= 0.0 {
        return 0.5;
    }
    // Draw `cap` combos ∝ reach (roulette), then take the unweighted mean equity.
    let mut sample = Vec::with_capacity(cap);
    for _ in 0..cap {
        let mut r = rng.random::<f32>() * total;
        let mut idx = combos.len() - 1;
        for (j, &w) in weights.iter().enumerate() {
            r -= w;
            if r < 0.0 {
                idx = j;
                break;
            }
        }
        sample.push(combos[idx]);
    }
    crate::eval::equity_vs_range(hero, board, &sample, iters)
}

/// The path token of a stored action label (the inverse of the generator's
/// label rendering): `"Fold"` → `"f"`, `"Call"` → `"c"`, `"Check"` → `"x"`,
/// `"Raise to 7.5bb"` → `"r7.5"`, `"All-in"` → `"ai"`.
pub fn label_token(label: &str) -> String {
    match label {
        "Fold" => "f".into(),
        "Call" => "c".into(),
        "Check" => "x".into(),
        "All-in" => "ai".into(),
        raise => format!(
            "r{}",
            raise
                .strip_prefix("Raise to ")
                .and_then(|s| s.strip_suffix("bb"))
                .unwrap_or("?")
        ),
    }
}

/// Provenance embedded in every ruleset's `header.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PreflopGenInfo {
    /// preflop-gen crate version.
    pub version: String,
    /// MCCFR traversals the solve ran.
    pub traversals: u64,
    /// RNG seed (solves are deterministic given seed + traversals).
    pub seed: u64,
    /// L∞ average-strategy drift over the last checkpoint interval, across
    /// exported nodes. `None` for solves too small to checkpoint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strategy_drift: Option<f32>,
}

/// `header.json`: identifies and dates a solved chart set.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PreflopHeader {
    /// Format version; readers reject what they don't know.
    pub version: u32,
    /// The ruleset id, e.g. `"cash89"` (also the directory name).
    pub ruleset: String,
    /// Human label, e.g. `"6-max cash, 89bb"`.
    pub label: String,
    /// Verbatim echo of the ruleset TOML this was solved under (provenance;
    /// the trainer never interprets it).
    pub config: serde_json::Value,
    /// Stable FNV-1a hash of the config echo — `gen` skips a ruleset whose
    /// existing header carries the same hash.
    pub config_hash: String,
    /// EV unit of every `evs` entry: `"bb"` (chip-EV) or `"payout"` (ICM, in
    /// units of the payout vector).
    pub ev_unit: String,
    /// Solve provenance.
    pub generator: PreflopGenInfo,
}

/// One decision node's equilibrium strategy for all 169 hand classes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PreflopNode {
    /// Action path from the root (see module docs). `""` = first to act.
    pub path: String,
    /// Seat to act, e.g. `"BB"`.
    pub seat: String,
    /// Pot at this decision (blinds + antes + all commitments), in bb.
    pub pot_bb: f32,
    /// What the acting seat must add to call, in bb.
    pub to_call_bb: f32,
    /// Arrival probability of this node under the equilibrium (max over the
    /// actor's classes) — the drill's sampling weight and the export prune key.
    pub reach: f32,
    /// Pre-rendered action labels, e.g. `"Fold"`, `"Call"`, `"Raise to 11bb"`,
    /// `"All-in"` — parallel to `freqs`/`evs`, same conventions as
    /// [`NodeStrategy`].
    pub actions: Vec<String>,
    /// Per action, 169 class frequencies in grid order, each class column
    /// summing to ~1.0.
    pub freqs: Vec<Vec<f32>>,
    /// Per action, 169 per-class EVs (unit: header `ev_unit`). Optional at
    /// the format level; always present in shipped data.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evs: Option<Vec<Vec<f32>>>,
}

impl PreflopNode {
    /// The [`NodeStrategy`] view of one class — plugs straight into the
    /// drills' EV-loss scoring. `None` when the file carries no EVs.
    pub fn strategy_for(&self, class: usize) -> Option<NodeStrategy> {
        let evs = self.evs.as_ref()?;
        Some(NodeStrategy {
            actions: self.actions.clone(),
            frequencies: self.freqs.iter().map(|f| f[class]).collect(),
            action_ev: evs.iter().map(|e| e[class]).collect(),
        })
    }

    /// This class's frequency of each action (grid order, parallel to
    /// `actions`).
    pub fn freqs_for(&self, class: usize) -> Vec<f32> {
        self.freqs.iter().map(|f| f[class]).collect()
    }
}

/// A ruleset's full chart set, indexed by action path.
#[derive(Debug)]
pub struct PreflopCharts {
    /// The ruleset's `header.json`.
    pub header: PreflopHeader,
    nodes: HashMap<String, PreflopNode>,
}

/// The newest format version this reader understands.
pub const FORMAT_VERSION: u32 = 1;

impl PreflopCharts {
    /// Load a ruleset directory: `header.json` + `starter.jsonl`, extended by
    /// `charts.jsonl` when a locally regenerated full export is present.
    pub fn load(dir: impl AsRef<Path>) -> io::Result<Self> {
        let dir = dir.as_ref();
        let read = |name: &str| {
            fs::read_to_string(dir.join(name)).map_err(|e| {
                io::Error::new(
                    e.kind(),
                    format!(
                        "{}: {e} — generate charts with \
                         `cargo run -p preflop-gen --release -- gen`",
                        dir.join(name).display()
                    ),
                )
            })
        };
        let bad = |msg: String| io::Error::new(io::ErrorKind::InvalidData, msg);
        let header: PreflopHeader =
            serde_json::from_str(&read("header.json")?).map_err(|e| bad(e.to_string()))?;
        if header.version > FORMAT_VERSION {
            return Err(bad(format!(
                "{}: format v{} is newer than this trainer understands (v{FORMAT_VERSION})",
                dir.display(),
                header.version
            )));
        }

        let mut nodes = HashMap::new();
        // charts.jsonl (the full local export) loads second so it wins.
        let full = dir.join("charts.jsonl");
        let mut files = vec![read("starter.jsonl")?];
        if full.exists() {
            files.push(read("charts.jsonl")?);
        }
        for text in files {
            for line in text.lines().filter(|l| !l.trim().is_empty()) {
                let node: PreflopNode =
                    serde_json::from_str(line).map_err(|e| bad(e.to_string()))?;
                let ok_shape = |m: &Vec<Vec<f32>>| {
                    m.len() == node.actions.len() && m.iter().all(|r| r.len() == CLASSES)
                };
                if !ok_shape(&node.freqs) || node.evs.as_ref().is_some_and(|e| !ok_shape(e)) {
                    return Err(bad(format!(
                        "node {:?}: freqs/evs shape doesn't match {} actions × {CLASSES} classes",
                        node.path,
                        node.actions.len()
                    )));
                }
                nodes.insert(node.path.clone(), node);
            }
        }
        Ok(Self { header, nodes })
    }

    /// Look up a node by its action path.
    pub fn node(&self, path: &str) -> Option<&PreflopNode> {
        self.nodes.get(path)
    }

    /// All stored nodes, unordered.
    pub fn nodes(&self) -> impl Iterator<Item = &PreflopNode> {
        self.nodes.values()
    }

    /// `seat`'s per-class arrival probability along `line`: the product of that
    /// seat's own past action frequencies. Unlike [`class_reach`](Self::class_reach)
    /// this names the seat explicitly and only reads the line's *ancestors*, so
    /// `line` may be a flop-closing line whose tail action has no decision node
    /// (the `export-range` case). `None` if an ancestor node is pruned/missing.
    // ponytail: blocker effects on reach are ignored — class-level by design.
    pub fn seat_reach(&self, line: &str, seat: &str) -> Option<Vec<f32>> {
        let mut reach = vec![1.0f32; CLASSES];
        let mut prefix = String::new();
        for tok in line.split('-').filter(|t| !t.is_empty()) {
            let node = self.node(&prefix)?;
            if node.seat == seat {
                let ai = node.actions.iter().position(|l| label_token(l) == tok)?;
                for (r, f) in reach.iter_mut().zip(&node.freqs[ai]) {
                    *r *= f;
                }
            }
            if !prefix.is_empty() {
                prefix.push('-');
            }
            prefix.push_str(tok);
        }
        Some(reach)
    }

    /// The acting seat's per-class arrival probability at `path` (the product
    /// of that seat's own past action frequencies). Requires a stored node at
    /// `path` to name the seat; use [`seat_reach`](Self::seat_reach) for a line
    /// past the last decision.
    pub fn class_reach(&self, path: &str) -> Option<Vec<f32>> {
        let seat = self.node(path)?.seat.clone();
        self.seat_reach(path, &seat)
    }

    /// How often a random deal travels `line` under the equilibrium: the
    /// product, over every seat that acts along it, of that seat's
    /// combo-weighted arrival marginal. Ranks lines for a precompute tier
    /// (design doc 08). `None` if an ancestor node is pruned/missing.
    // ponytail: product of marginals — class-level card removal and cross-seat
    // hand correlation are ignored, like everything in these charts. Fine for
    // ranking; not a simulator.
    pub fn line_mass(&self, line: &str) -> Option<f32> {
        let mut seats: Vec<String> = Vec::new();
        let mut prefix = String::new();
        for tok in line.split('-').filter(|t| !t.is_empty()) {
            let node = self.node(&prefix)?;
            if !seats.contains(&node.seat) {
                seats.push(node.seat.clone());
            }
            if !prefix.is_empty() {
                prefix.push('-');
            }
            prefix.push_str(tok);
        }
        let mut mass = 1.0f32;
        for seat in &seats {
            let reach = self.seat_reach(line, seat)?;
            let combos: f32 = reach
                .iter()
                .enumerate()
                .map(|(i, r)| class_combos(i) as f32 * r)
                .sum();
            mass *= combos / 1326.0;
        }
        Some(mass)
    }

    /// A ruleset-config number, e.g. `stack_bb`, `bb`, `ante_bb`.
    fn cfg_f32(&self, key: &str) -> Option<f32> {
        self.header
            .config
            .get(key)
            .and_then(|v| v.as_f64())
            .map(|v| v as f32)
    }

    /// Starting stack (bb) from the ruleset config.
    pub fn stack_bb(&self) -> Option<f32> {
        self.cfg_f32("stack_bb")
    }

    /// Postflop rake `(rate, cap_bb)` from the ruleset config (0 if unset) — the
    /// same rake the preflop equilibrium was solved under.
    pub fn rake(&self) -> (f32, f32) {
        (
            self.cfg_f32("rake_rate").unwrap_or(0.0),
            self.cfg_f32("rake_cap_bb").unwrap_or(0.0),
        )
    }

    /// The two seats that reach the flop on `line`, as `(oop, ip)` ordered by
    /// postflop action (the button acts last). `None` unless exactly two seats
    /// stay live (i.e. `line` is a clean two-player flop) and the seats config
    /// is present. Card-removal aside, this is position-only.
    pub fn flop_seats(&self, line: &str) -> Option<(String, String)> {
        let seats: Vec<String> = self
            .header
            .config
            .get("seats")?
            .as_array()?
            .iter()
            .filter_map(|s| s.as_str().map(String::from))
            .collect();
        // Replay the line's fold tokens to find who's still in at the flop.
        let mut folded = std::collections::HashSet::new();
        let mut prefix = String::new();
        for tok in line.split('-').filter(|t| !t.is_empty()) {
            let node = self.node(&prefix)?;
            if tok == "f" {
                folded.insert(node.seat.clone());
            }
            if !prefix.is_empty() {
                prefix.push('-');
            }
            prefix.push_str(tok);
        }
        let live: Vec<&String> = seats.iter().filter(|s| !folded.contains(*s)).collect();
        if live.len() != 2 {
            return None;
        }
        // Postflop order = the preflop seat order rotated so the seat after the
        // button leads and the button acts last. The button is BTN, or the SB
        // heads-up (where the SB *is* the button).
        let btn = if seats.iter().any(|s| s == "BTN") {
            "BTN"
        } else {
            "SB"
        };
        let bi = seats.iter().position(|s| s == btn)?;
        let order: Vec<&String> = seats[bi + 1..].iter().chain(&seats[..=bi]).collect();
        let rank = |s: &String| order.iter().position(|p| *p == s);
        if rank(live[0])? <= rank(live[1])? {
            Some((live[0].clone(), live[1].clone()))
        } else {
            Some((live[1].clone(), live[0].clone()))
        }
    }

    /// Pot (bb) when `line` closes to a flop: the last decision's `pot_bb` for a
    /// check-through, `pot_bb + to_call_bb` for a called raise. `None` if the
    /// line doesn't close to a flop — a decision still follows (incl. the root
    /// SB limp), or it's a fold/all-in terminal. Trailing folds are stripped
    /// first: a cold-caller can leave the blinds to fold behind them, and those
    /// blinds only forfeit chips already posted into the closing pot, so the
    /// last call/check still sets it (e.g. `r2-f-f-c-f-f` = open, BTN cold-call,
    /// blinds fold — the pot is fixed by BTN's call).
    pub fn flop_pot_bb(&self, line: &str) -> Option<f32> {
        if self.node(line).is_some() {
            return None; // a decision follows — not a flop yet
        }
        let closed = line.trim_end_matches("-f");
        let (parent, last) = closed.rsplit_once('-').unwrap_or(("", closed));
        let node = self.node(parent)?;
        match last {
            "x" => Some(node.pot_bb),
            "c" => Some(node.pot_bb + node.to_call_bb),
            _ => None,
        }
    }

    /// Each live player's total preflop commitment (bb) on `line`: the highest
    /// bet-to level reached — both live players matched it to see the flop —
    /// floored at the big blind, plus any ante. Effective postflop stack is
    /// [`stack_bb`](Self::stack_bb) minus this (heads-up: `stack − pot/2`; with
    /// a folded blind's dead money, `stack − raise_to ≠ pot/2`).
    // ponytail: ante is a flat per-player post; assumes equal starting stacks —
    // per-seat commitment tracking if we model short-stack/ICM spots.
    pub fn line_commitment_bb(&self, line: &str) -> f32 {
        let bb = self.cfg_f32("bb").unwrap_or(1.0);
        let ante = self.cfg_f32("ante_bb").unwrap_or(0.0);
        let max_to = line
            .split('-')
            .filter_map(|t| t.strip_prefix('r'))
            .filter_map(|s| s.parse::<f32>().ok())
            .fold(bb, f32::max);
        max_to + ante
    }

    /// Every two-player action line that closes to a flop, as `(line, pot_bb)`,
    /// sorted — i.e. exactly the lines `--line`/`export-range` can solve (the
    /// solver is heads-up). Multiway closes, folds, and all-ins drop out.
    // ponytail: a pruned continuation can masquerade as a flop close; fine for
    // the committed starter tier — regenerate charts.jsonl for rarer/deeper lines.
    pub fn flop_lines(&self) -> Vec<(String, f32)> {
        let mut out = Vec::new();
        self.collect_flop_lines("", &mut out);
        out.retain(|(line, _)| self.flop_seats(line).is_some());
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out
    }

    fn collect_flop_lines(&self, path: &str, out: &mut Vec<(String, f32)>) {
        let Some(node) = self.node(path) else { return };
        for label in &node.actions {
            let tok = label_token(label);
            // Fold branches are walked (a cold-caller's blinds fold behind them,
            // still reaching a flop — see `flop_pot_bb`); all-in ends the hand.
            if tok == "ai" {
                continue;
            }
            let child = if path.is_empty() {
                tok.clone()
            } else {
                format!("{path}-{tok}")
            };
            if self.node(&child).is_some() {
                self.collect_flop_lines(&child, out);
            } else if let Some(pot) = self.flop_pot_bb(&child) {
                out.push((child, pot));
            }
        }
    }
}

/// Render a per-class arrival `reach` (0..) as a weighted solver range string,
/// `"AA:0.6200,AKs:0.8000,…"` — one entry per class with positive reach, each
/// weight scaled so the range's max is 1 (absolute scale doesn't change the
/// equilibrium, and this uses the solver's full `[0,1]` resolution). Combos
/// whose scaled weight rounds below `0.0001` drop out. Empty if every class has
/// zero reach.
pub fn weighted_range_string(reach: &[f32]) -> String {
    let max = reach.iter().copied().fold(0.0f32, f32::max);
    if max <= 0.0 {
        return String::new();
    }
    reach
        .iter()
        .take(CLASSES)
        .enumerate()
        .filter_map(|(i, &w)| {
            let scaled = w / max;
            (scaled >= 1e-4).then(|| format!("{}:{scaled:.4}", class_name(i)))
        })
        .collect::<Vec<_>>()
        .join(",")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn class_names_round_trip_in_grid_order() {
        for i in 0..CLASSES {
            let name = class_name(i);
            assert_eq!(class_index_of(&name), Some(i), "class {i} = {name}");
        }
        assert_eq!(class_name(0), "AA");
        assert_eq!(class_name(1), "AKs");
        assert_eq!(class_name(13), "AKo");
        assert_eq!(class_name(168), "22");
        assert_eq!(class_index_of("KAs"), None); // wrong rank order
        assert_eq!(class_index_of("AKx"), None);
        assert_eq!(class_index_of("AAs"), None); // pairs carry no suffix
    }

    #[test]
    fn class_index_maps_combos_to_cells() {
        let idx = |a, b| class_index([Card::try_from(a).unwrap(), Card::try_from(b).unwrap()]);
        assert_eq!(idx("As", "Ah"), 0); // AA
        assert_eq!(idx("As", "Ks"), 1); // AKs
        assert_eq!(idx("Ks", "Ad"), 13); // AKo, order-independent
        assert_eq!(idx("7d", "2c"), class_index_of("72o").unwrap());
        assert_eq!(idx("2c", "2d"), 168);
    }

    #[test]
    fn parse_cards_reads_hands_and_boards() {
        assert_eq!(parse_cards("AhKh").map(|c| c.len()), Some(2));
        assert_eq!(parse_cards("Td 9d 6h").map(|c| c.len()), Some(3));
        assert_eq!(parse_cards("AhK"), None); // odd length
        assert_eq!(parse_cards("Zx"), None); // bad rank/suit
        assert_eq!(parse_cards(""), None);
        let h = parse_cards("AsKs").unwrap();
        assert_eq!(class_index([h[0], h[1]]), class_index_of("AKs").unwrap());
    }

    #[test]
    fn class_combos_partition_the_deck() {
        let total: u32 = (0..CLASSES).map(class_combos).sum();
        assert_eq!(total, 1326); // C(52, 2)
        assert_eq!(class_combos(0), 6);
        assert_eq!(class_combos(1), 4);
        assert_eq!(class_combos(13), 12);
    }

    #[test]
    fn class_combos_cards_round_trip() {
        for i in 0..CLASSES {
            let combos = class_combos_cards(i);
            assert_eq!(
                combos.len() as u32,
                class_combos(i),
                "class {i} combo count"
            );
            for combo in combos {
                let idx = class_index(combo);
                assert_eq!(idx, i, "{combo:?} should map back to class {i}");
            }
        }
    }

    #[test]
    fn equity_vs_reach_reads_the_range() {
        let card = |s| Card::try_from(s).unwrap();
        let hero = [card("Ah"), card("As")];
        let flop = [card("Kd"), card("Qc"), card("7h")];
        let mut rng = rand::rng();

        // Villain always holds 22 — trip-less AA on K Q 7 should dominate.
        let mut reach = vec![0.0f32; CLASSES];
        reach[class_index_of("22").unwrap()] = 1.0;
        let eq = equity_vs_reach(hero, &flop, &reach, &mut rng, 200, 60);
        assert!(
            eq > 0.8,
            "AA vs a pure-22 range on K Q 7 should dominate: {eq}"
        );

        // Empty range (all reach zero) => the 0.5 fallback.
        let zero = vec![0.0f32; CLASSES];
        assert_eq!(equity_vs_reach(hero, &flop, &zero, &mut rng, 50, 20), 0.5);
    }

    fn sample_header() -> PreflopHeader {
        PreflopHeader {
            version: FORMAT_VERSION,
            ruleset: "test".into(),
            label: "test ruleset".into(),
            config: serde_json::json!({"stack_bb": 100.0}),
            config_hash: "00000000".into(),
            ev_unit: "bb".into(),
            generator: PreflopGenInfo {
                version: "0.1.0".into(),
                traversals: 1,
                seed: 1,
                strategy_drift: None,
            },
        }
    }

    fn sample_node(path: &str, evs: bool) -> PreflopNode {
        PreflopNode {
            path: path.into(),
            seat: "BB".into(),
            pot_bb: 3.5,
            to_call_bb: 1.5,
            reach: 0.4,
            actions: vec!["Fold".into(), "Call".into()],
            freqs: vec![vec![0.25; CLASSES], vec![0.75; CLASSES]],
            evs: evs.then(|| vec![vec![0.0; CLASSES], vec![1.5; CLASSES]]),
        }
    }

    fn write_ruleset(dir: &Path, nodes: &[PreflopNode]) {
        fs::create_dir_all(dir).unwrap();
        fs::write(
            dir.join("header.json"),
            serde_json::to_string(&sample_header()).unwrap(),
        )
        .unwrap();
        let lines: Vec<String> = nodes
            .iter()
            .map(|n| serde_json::to_string(n).unwrap())
            .collect();
        fs::write(dir.join("starter.jsonl"), lines.join("\n")).unwrap();
    }

    #[test]
    fn load_round_trips_and_scores_ev_loss() {
        let dir = std::env::temp_dir().join(format!("pt-preflop-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        write_ruleset(&dir, &[sample_node("", true), sample_node("f-f", false)]);

        let charts = PreflopCharts::load(&dir).unwrap();
        assert_eq!(charts.header.ruleset, "test");
        assert_eq!(charts.nodes().count(), 2);

        let ns = charts.node("").unwrap().strategy_for(0).unwrap();
        assert_eq!(ns.best(), 1); // Call: 1.5bb over Fold's 0.0
        assert!((ns.ev_loss(0) - 1.5).abs() < 1e-6);
        // EV-less nodes still expose frequencies, just no NodeStrategy.
        assert!(charts.node("f-f").unwrap().strategy_for(0).is_none());
        assert_eq!(charts.node("f-f").unwrap().freqs_for(3), vec![0.25, 0.75]);
        assert!(charts.node("nope").is_none());

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn load_rejects_bad_shapes_and_newer_versions() {
        let dir = std::env::temp_dir().join(format!("pt-preflop-bad-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let mut node = sample_node("", true);
        node.freqs.pop(); // 1 freq row for 2 actions
        write_ruleset(&dir, &[node]);
        let err = PreflopCharts::load(&dir).unwrap_err();
        assert!(err.to_string().contains("shape"), "{err}");

        let mut header = sample_header();
        header.version = FORMAT_VERSION + 1;
        fs::write(
            dir.join("header.json"),
            serde_json::to_string(&header).unwrap(),
        )
        .unwrap();
        let err = PreflopCharts::load(&dir).unwrap_err();
        assert!(err.to_string().contains("newer"), "{err}");

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn label_tokens_invert_the_labels() {
        for (label, tok) in [
            ("Fold", "f"),
            ("Call", "c"),
            ("Check", "x"),
            ("All-in", "ai"),
            ("Raise to 2.5bb", "r2.5"),
            ("Raise to 17.25bb", "r17.25"),
        ] {
            assert_eq!(label_token(label), tok);
        }
    }

    #[test]
    fn class_reach_multiplies_own_past_frequencies() {
        let dir = std::env::temp_dir().join(format!("pt-preflop-reach-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        // Synthetic 3-node line where the same seat acts at "" and "c-c":
        // reach at "c-c" is the root call frequency (0.75 everywhere).
        let mut root = sample_node("", false);
        root.seat = "SB".into();
        let mut mid = sample_node("c", false);
        mid.seat = "BB".into();
        let mut back = sample_node("c-c", false);
        back.seat = "SB".into();
        write_ruleset(&dir, &[root, mid, back]);

        let charts = PreflopCharts::load(&dir).unwrap();
        let reach = charts.class_reach("c-c").unwrap();
        assert!((reach[0] - 0.75).abs() < 1e-6);
        // The seat acting at "c" never acted before: full reach.
        assert!(charts.class_reach("c").unwrap().iter().all(|&r| r == 1.0));
        assert!(charts.class_reach("nope").is_none());

        fs::remove_dir_all(&dir).unwrap();
    }

    /// A tiny heads-up tree: SB opens 2.5bb (or folds) at the root, BB calls
    /// (or folds); `r2.5-c` closes to a flop. Distinct freqs per seat/action so
    /// the reach products are checkable.
    fn write_hu_ruleset(dir: &Path) {
        fs::create_dir_all(dir).unwrap();
        let mut header = sample_header();
        header.ruleset = "hu".into();
        header.config = serde_json::json!({
            "seats": ["SB", "BB"],
            "sb": 0.5, "bb": 1.0, "ante_bb": 0.0, "stack_bb": 55.0,
            "rake_rate": 0.05, "rake_cap_bb": 3.0,
        });
        fs::write(
            dir.join("header.json"),
            serde_json::to_string(&header).unwrap(),
        )
        .unwrap();
        let mk = |path: &str, seat: &str, actions: Vec<&str>, freqs: Vec<f32>, to_call: f32| {
            PreflopNode {
                path: path.into(),
                seat: seat.into(),
                pot_bb: 3.5,
                to_call_bb: to_call,
                reach: 1.0,
                actions: actions.into_iter().map(String::from).collect(),
                freqs: freqs.iter().map(|&f| vec![f; CLASSES]).collect(),
                evs: None,
            }
        };
        let nodes = [
            mk(
                "",
                "SB",
                vec!["Fold", "Raise to 2.5bb"],
                vec![0.4, 0.6],
                0.0,
            ),
            mk("r2.5", "BB", vec!["Fold", "Call"], vec![0.3, 0.7], 1.5),
        ];
        let lines: Vec<String> = nodes
            .iter()
            .map(|n| serde_json::to_string(n).unwrap())
            .collect();
        fs::write(dir.join("starter.jsonl"), lines.join("\n")).unwrap();
    }

    #[test]
    fn export_range_bridges_a_hu_line() {
        let dir = std::env::temp_dir().join(format!("pt-preflop-export-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        write_hu_ruleset(&dir);
        let charts = PreflopCharts::load(&dir).unwrap();
        let line = "r2.5-c";

        // Each seat's reach is the product of *its own* action frequencies.
        assert!(charts
            .seat_reach(line, "SB")
            .unwrap()
            .iter()
            .all(|&r| (r - 0.6).abs() < 1e-6));
        assert!(charts
            .seat_reach(line, "BB")
            .unwrap()
            .iter()
            .all(|&r| (r - 0.7).abs() < 1e-6));

        // OOP=BB, IP=SB (the SB is the button heads-up).
        assert_eq!(charts.flop_seats(line), Some(("BB".into(), "SB".into())));

        // Pot = both players' 2.5bb; effective stack = 55 − 2.5 = 52.5 = stack − pot/2.
        let pot = charts.flop_pot_bb(line).unwrap();
        assert!((pot - 5.0).abs() < 1e-6);
        assert!((charts.line_commitment_bb(line) - 2.5).abs() < 1e-6);
        assert!((charts.stack_bb().unwrap() - pot / 2.0 - 52.5).abs() < 1e-6);
        assert_eq!(charts.rake(), (0.05, 3.0));

        // The line is discoverable and non-flop-closing paths are rejected.
        assert_eq!(charts.flop_lines(), vec![("r2.5-c".to_string(), 5.0)]);
        assert!(charts.flop_pot_bb("r2.5").is_none()); // a decision still follows
        assert!(charts.flop_seats("r2.5-f").is_none()); // BB folds — one live seat

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn cold_caller_leaves_a_two_player_flop_when_blinds_fold_behind() {
        // BTN opens 2bb, SB cold-calls, BB folds behind: a heads-up BTN-vs-SB
        // flop whose closing action is BB's fold, not a call/check.
        let dir = std::env::temp_dir().join(format!("pt-preflop-coldcall-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let mut header = sample_header();
        header.config = serde_json::json!({"seats": ["BTN", "SB", "BB"], "stack_bb": 100.0});
        fs::write(
            dir.join("header.json"),
            serde_json::to_string(&header).unwrap(),
        )
        .unwrap();
        let mk = |path: &str, seat: &str, pot: f32, to_call: f32, actions: Vec<&str>| PreflopNode {
            path: path.into(),
            seat: seat.into(),
            pot_bb: pot,
            to_call_bb: to_call,
            reach: 1.0,
            actions: actions.iter().map(|s| s.to_string()).collect(),
            freqs: vec![vec![0.5; CLASSES]; actions.len()],
            evs: None,
        };
        let nodes = [
            mk("", "BTN", 1.5, 0.0, vec!["Fold", "Raise to 2bb"]),
            mk("r2", "SB", 3.5, 1.5, vec!["Fold", "Call"]),
            mk("r2-c", "BB", 5.0, 1.0, vec!["Fold", "Call"]),
        ];
        let lines: Vec<String> = nodes
            .iter()
            .map(|n| serde_json::to_string(n).unwrap())
            .collect();
        fs::write(dir.join("starter.jsonl"), lines.join("\n")).unwrap();

        let charts = PreflopCharts::load(&dir).unwrap();
        // Trailing folds are stripped: the pot is the one after SB's call
        // (BTN 2 + SB 2 + BB's dead 1), and the button acts last postflop.
        assert_eq!(charts.flop_pot_bb("r2-c-f"), Some(5.0));
        assert_eq!(
            charts.flop_seats("r2-c-f"),
            Some(("SB".into(), "BTN".into()))
        );
        // Only the heads-up close is offered; `r2-c` (BB still to act) and the
        // 3-way `r2-c-c` are not.
        assert_eq!(charts.flop_lines(), vec![("r2-c-f".to_string(), 5.0)]);
        assert!(charts.flop_pot_bb("r2-c").is_none());

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn line_commitment_is_max_bet_to_plus_ante() {
        let dir = std::env::temp_dir().join(format!("pt-preflop-commit-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        write_hu_ruleset(&dir);
        let charts = PreflopCharts::load(&dir).unwrap();
        // Highest bet-to level wins; a folded blind is dead money, not commitment,
        // so a 100bb open-fold-call commits only the raise-to (≠ pot/2).
        assert!((charts.line_commitment_bb("f-f-f-r2.5-f-c") - 2.5).abs() < 1e-6);
        assert!((charts.line_commitment_bb("r2.5-r8-c") - 8.0).abs() < 1e-6);
        assert!((charts.line_commitment_bb("c-x") - 1.0).abs() < 1e-6); // limp: floor at bb
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn weighted_range_string_scales_to_max_one() {
        let mut reach = vec![0.0f32; CLASSES];
        reach[0] = 0.6; // AA
        reach[1] = 0.3; // AKs
        assert_eq!(weighted_range_string(&reach), "AA:1.0000,AKs:0.5000");
        assert_eq!(weighted_range_string(&vec![0.0; CLASSES]), "");
    }

    #[test]
    fn load_missing_dir_hints_at_the_generator() {
        let err = PreflopCharts::load("data/preflop/definitely-not-a-ruleset").unwrap_err();
        assert!(err.to_string().contains("preflop-gen"), "{err}");
    }

    #[test]
    fn header_with_unknown_fields_still_parses() {
        // Forward compat: same-version files may grow additive fields.
        let mut v = serde_json::to_value(sample_header()).unwrap();
        v["future_field"] = serde_json::json!(42);
        let back: PreflopHeader = serde_json::from_value(v).unwrap();
        assert_eq!(back.ruleset, "test");
    }
}
