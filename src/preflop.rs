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
    /// The ruleset id, e.g. `"cash100"` (also the directory name).
    pub ruleset: String,
    /// Human label, e.g. `"6-max cash, 100bb"`.
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
