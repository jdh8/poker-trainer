//! Reach-pruned postflop tables: the multi-street analog of the preflop chart
//! format ([`crate::preflop`]). `crates/solve-gen tables` walks a solved game
//! and stores the strategy at frequently-reached nodes only — path-addressed,
//! ~MB per flop instead of the ~10 GB full game — and the trainer reads them
//! for offline `drill hand` / `table` browsing, live-solving off the pruned
//! path (design memo: postflop-table-direction).
//!
//! One config's tables live under `data/tables/<formation>/`: a
//! `header-<hash8>.json` (config echo + provenance) plus one
//! `<flop>-<hash8>.jsonl` per flop, one [`TableNode`] per line. Nodes are keyed
//! by their action line — the same display labels a [`crate::tree::TreeNode`]
//! carries (`["Check", "Bet 2.0bb", "deal 2c"]`) — so no new address grammar
//! exists. The format lives here, trainer-side, so the AGPL generator depends
//! on us and links no solver into the trainer (same seam as [`crate::preflop`]).

use crate::solution::{GenInfo, SpotConfig};
use crate::tree::TreeNode;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::Path;

/// The newest table format this reader understands.
pub const FORMAT_VERSION: u32 = 1;

/// The on-disk directory name for a formation id. Grounded ids embed a `:`
/// (`"cash-hu55:r2.5-c"`), which Windows filenames forbid — swap it for `_`,
/// which appears in neither formation ids nor line tokens. A no-op for the
/// curated formations, so existing table trees stay valid. Headers and
/// configs keep the raw id; only the path is sanitized (generator write side
/// and trainer read side both come through here).
pub fn formation_dir(formation: &str) -> String {
    formation.replace(':', "_")
}

/// Join key separator for an action line — a unit separator, never a label char.
const SEP: &str = "\u{1f}";

/// One stored node: a [`TreeNode`] plus its arrival probability along the
/// betting path (chance deals don't penalize reach; see the generator).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TableNode {
    /// Arrival probability of this node under the equilibrium (the prune key).
    pub reach: f32,
    /// The node itself — flattened so a row reads like a plain `TreeNode` + reach.
    #[serde(flatten)]
    pub node: TreeNode,
}

/// `header-<hash8>.json`: identifies and dates one config's tables.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TableHeader {
    /// Format version; readers reject what they don't know.
    pub version: u32,
    /// Formation id, e.g. `"srp-btn-bb"` (also the directory name).
    pub formation: String,
    /// The game config these tables were solved under.
    pub config: SpotConfig,
    /// `config.hash8()` — the cache key in every filename here.
    pub config_hash: String,
    /// Solve provenance (crate version + exploitability reached).
    pub generator: GenInfo,
    /// Reach threshold the walk pruned at — subtrees below it live-solve.
    pub reach: f32,
}

/// One config's reach-pruned tables for one flop, indexed by action line.
#[derive(Debug)]
pub struct PostflopTable {
    /// The config's `header-<hash8>.json`.
    pub header: TableHeader,
    nodes: HashMap<String, TableNode>,
}

impl PostflopTable {
    /// Load `<dir>/header-<hash8>.json` + `<dir>/<flop>-<hash8>.jsonl`. `dir` is
    /// the formation directory (`data/tables/<formation>`). `Err` — missing,
    /// too-new, or malformed — is the caller's cue to live-solve instead.
    pub fn load(dir: impl AsRef<Path>, flop: &str, hash8: &str) -> io::Result<Self> {
        let dir = dir.as_ref();
        let bad = |msg: String| io::Error::new(io::ErrorKind::InvalidData, msg);

        let header_path = dir.join(format!("header-{hash8}.json"));
        let header: TableHeader =
            serde_json::from_str(&fs::read_to_string(&header_path).map_err(|e| {
                io::Error::new(
                    e.kind(),
                    format!(
                        "{}: {e} — generate with `cargo run -p solve-gen --release -- tables`",
                        header_path.display()
                    ),
                )
            })?)
            .map_err(|e| bad(e.to_string()))?;
        if header.version > FORMAT_VERSION {
            return Err(bad(format!(
                "{}: table format v{} is newer than this trainer understands (v{FORMAT_VERSION})",
                header_path.display(),
                header.version
            )));
        }

        let jsonl = dir.join(format!("{}-{hash8}.jsonl", flop.to_lowercase()));
        let text = fs::read_to_string(&jsonl)
            .map_err(|e| io::Error::new(e.kind(), format!("{}: {e}", jsonl.display())))?;
        let mut nodes = HashMap::new();
        for line in text.lines().filter(|l| !l.trim().is_empty()) {
            let tn: TableNode = serde_json::from_str(line).map_err(|e| bad(e.to_string()))?;
            nodes.insert(tn.node.line.join(SEP), tn);
        }
        Ok(Self { header, nodes })
    }

    /// The stored node at an action `line`, e.g. `["Check", "deal 2c"]`. The
    /// root is `&[]`. `None` for a pruned/off-path node (the live-solve cue).
    pub fn node(&self, line: &[String]) -> Option<&TableNode> {
        self.nodes.get(&line.join(SEP))
    }

    /// How many nodes are stored.
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    /// All stored nodes, unordered.
    pub fn nodes(&self) -> impl Iterator<Item = &TableNode> {
        self.nodes.values()
    }

    /// Whether the table stored no nodes.
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_config() -> SpotConfig {
        SpotConfig {
            formation: "srp-btn-bb".into(),
            oop_range: "AA,KK".into(),
            ip_range: "QQ,JJ".into(),
            flop_sizes: "50%".into(),
            turn_sizes: "33%".into(),
            river_sizes: "33%".into(),
            stack_bb: 97.0,
            pot_bb: 6.0,
            rake_rate: 0.0,
            rake_cap_bb: 0.0,
        }
    }

    fn sample_header(config: &SpotConfig) -> TableHeader {
        TableHeader {
            version: FORMAT_VERSION,
            formation: config.formation.clone(),
            config: config.clone(),
            config_hash: config.hash8(),
            generator: GenInfo {
                version: "0.1.0".into(),
                exploitability_bb: 0.02,
            },
            reach: 0.002,
        }
    }

    /// A minimal OOP player node at the root (empty line).
    fn root_node() -> TableNode {
        TableNode {
            reach: 1.0,
            node: TreeNode {
                player: "oop".into(),
                board: vec!["6h".into(), "9d".into(), "Td".into()],
                pot_bb: 6.0,
                line: vec![],
                actions: vec!["Check".into(), "Bet 3.0bb".into()],
                dealable: vec![],
                hands: vec!["AsKs".into()],
                freqs: vec![vec![0.6], vec![0.4]],
                evs: vec![vec![1.0], vec![1.5]],
                weights: vec![1.0],
                equity: vec![0.55],
            },
        }
    }

    /// One step down: IP faces the check.
    fn ip_node() -> TableNode {
        TableNode {
            reach: 0.6,
            node: TreeNode {
                player: "ip".into(),
                board: vec!["6h".into(), "9d".into(), "Td".into()],
                pot_bb: 6.0,
                line: vec!["Check".into()],
                actions: vec!["Check".into(), "Bet 2.0bb".into()],
                dealable: vec![],
                hands: vec!["QdQc".into()],
                freqs: vec![vec![0.3], vec![0.7]],
                evs: vec![vec![2.0], vec![2.4]],
                weights: vec![1.0],
                equity: vec![0.6],
            },
        }
    }

    fn write_table(dir: &Path, flop: &str, header: &TableHeader, nodes: &[TableNode]) {
        fs::create_dir_all(dir).unwrap();
        fs::write(
            dir.join(format!("header-{}.json", header.config_hash)),
            serde_json::to_string_pretty(header).unwrap(),
        )
        .unwrap();
        let lines: Vec<String> = nodes
            .iter()
            .map(|n| serde_json::to_string(n).unwrap())
            .collect();
        fs::write(
            dir.join(format!(
                "{}-{}.jsonl",
                flop.to_lowercase(),
                header.config_hash
            )),
            lines.join("\n"),
        )
        .unwrap();
    }

    #[test]
    fn load_round_trips_and_looks_up_by_line() {
        let config = sample_config();
        let dir = std::env::temp_dir().join(format!("pt-table-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        write_table(
            &dir,
            "Td9d6h",
            &sample_header(&config),
            &[root_node(), ip_node()],
        );

        let table = PostflopTable::load(&dir, "Td9d6h", &config.hash8()).unwrap();
        assert_eq!(table.header.formation, "srp-btn-bb");
        assert_eq!(table.len(), 2);

        // Root at the empty line; the child under its action label.
        let root = table.node(&[]).unwrap();
        assert_eq!(root.node.player, "oop");
        assert_eq!(root.reach, 1.0);
        let ip = table.node(&["Check".to_string()]).unwrap();
        assert_eq!(ip.node.player, "ip");
        assert_eq!(ip.node.hands, vec!["QdQc"]);
        assert!((ip.reach - 0.6).abs() < 1e-6);
        // An off-path line is absent (the live-solve cue).
        assert!(table.node(&["Bet 3.0bb".to_string()]).is_none());

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn load_rejects_newer_versions_and_missing_files() {
        let config = sample_config();
        let dir = std::env::temp_dir().join(format!("pt-table-ver-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let mut header = sample_header(&config);
        header.version = FORMAT_VERSION + 1;
        write_table(&dir, "Td9d6h", &header, &[root_node()]);
        let err = PostflopTable::load(&dir, "Td9d6h", &config.hash8()).unwrap_err();
        assert!(err.to_string().contains("newer"), "{err}");

        // A different config-hash has no header here — the live-solve cue.
        let err = PostflopTable::load(&dir, "Td9d6h", "deadbeef").unwrap_err();
        assert!(err.to_string().contains("solve-gen"), "{err}");

        fs::remove_dir_all(&dir).unwrap();
    }
}
