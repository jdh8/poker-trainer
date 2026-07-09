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

    /// The acting seat's per-class arrival probability at `path`: the product
    /// of that seat's own past action frequencies along the line (export
    /// prunes children before parents, so every stored node's ancestors are
    /// stored too).
    // ponytail: blocker effects on reach are ignored — class-level by design.
    pub fn class_reach(&self, path: &str) -> Option<Vec<f32>> {
        let seat = &self.node(path)?.seat;
        let mut reach = vec![1.0f32; CLASSES];
        let mut prefix = String::new();
        for tok in path.split('-').filter(|t| !t.is_empty()) {
            let node = self.node(&prefix)?;
            if &node.seat == seat {
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
