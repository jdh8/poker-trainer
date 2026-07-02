//! Tree sessions: query arbitrary nodes of a solved game (design doc 01, P4).
//!
//! A [`TreeSession`] holds a long-lived `solve-gen serve` subprocess with the
//! solved game resident in memory, and navigates it over line-delimited JSON on
//! stdio. The [`TreeNode`] payload defined here — not the solver's own tree
//! format — is the wire format (protocol v2, see [`PROTOCOL_V`]), which keeps
//! the AGPL solver behind a process boundary exactly like the snapshot path.

use crate::solution::{solve_gen_command, HandStrategy, NodeStrategy, SolveRequest, SolvedSpot};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::io::{self, BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Stdio};

/// Protocol version sent with `op:solve`; serve rejects versions it can't
/// speak. v2: the solve body is a full [`SolveRequest`] (flop + `SpotConfig`)
/// instead of v1's sparse overrides.
pub const PROTOCOL_V: u32 = 2;

/// One node of the solved tree, as served by `solve-gen serve`.
///
/// `freqs` and `evs` are `[action][hand]`, parallel to `actions` × `hands`;
/// EVs are in bb. All strategy fields are empty at chance/terminal nodes, and
/// `dealable` is empty except at chance nodes.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TreeNode {
    /// `"oop"`, `"ip"`, `"chance"`, or `"terminal"`.
    pub player: String,
    /// Board so far, e.g. `["Td","9d","6h","2c"]`.
    pub board: Vec<String>,
    pub pot_bb: f32,
    /// Action labels from the root, for display: `["Check","Bet 2.0bb","deal 2c"]`.
    pub line: Vec<String>,
    /// The acting player's action labels (empty at chance/terminal nodes).
    pub actions: Vec<String>,
    /// Cards that can be dealt (chance nodes only).
    #[serde(default)]
    pub dealable: Vec<String>,
    #[serde(default)]
    pub hands: Vec<String>,
    #[serde(default)]
    pub freqs: Vec<Vec<f32>>,
    #[serde(default)]
    pub evs: Vec<Vec<f32>>,
    /// Reach weight per hand (combo mass at this node), parallel to `hands`.
    #[serde(default)]
    pub weights: Vec<f32>,
    /// Equity vs. the villain's reaching range, parallel to `hands`.
    #[serde(default)]
    pub equity: Vec<f32>,
}

/// One dealable card's summary from the `runouts` op: the next player's
/// reach-weighted aggregate action mix and EV after that card falls.
#[derive(Debug, Clone, Deserialize)]
pub struct RunoutSummary {
    pub card: String,
    pub actions: Vec<String>,
    pub freqs: Vec<f32>,
    pub ev_bb: f32,
}

impl TreeNode {
    /// Reshape into a [`SolvedSpot`] so the existing grid/render code applies
    /// unchanged — a `TreeNode` and a snapshot reduce to the same `Cell` grid.
    pub fn to_spot(&self) -> SolvedSpot {
        let strategies = self
            .hands
            .iter()
            .enumerate()
            .map(|(j, hand)| HandStrategy {
                hand: hand.clone(),
                strategy: NodeStrategy {
                    actions: self.actions.clone(),
                    frequencies: self.freqs.iter().map(|per_hand| per_hand[j]).collect(),
                    action_ev: self.evs.iter().map(|per_hand| per_hand[j]).collect(),
                },
            })
            .collect();
        SolvedSpot {
            label: if self.line.is_empty() {
                "(root)".into()
            } else {
                self.line.join(" · ")
            },
            board: self.board.clone(),
            pot_bb: self.pot_bb,
            hero_oop: self.player == "oop",
            villain_action: self.line.last().cloned().unwrap_or_default(),
            config: None,
            generator: None,
            strategies,
        }
    }
}

/// A live `solve-gen serve` subprocess holding one solved game.
///
/// Concrete struct, no trait: `SolutionProvider` keeps serving snapshot drills,
/// and this is the only tree source (design doc 01). The child is killed on
/// `Drop`; a dead/garbled child surfaces as an `io::Error` from any op.
pub struct TreeSession {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

impl TreeSession {
    /// Spawn `solve-gen serve`, solve `req` (expect ~30 s and ~1 GB RAM for an
    /// uncached spot; progress prints on stderr), and return the root node.
    pub fn start(req: &SolveRequest) -> io::Result<(Self, TreeNode)> {
        let mut child = solve_gen_command(&["serve".into()])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()?;
        let stdin = child.stdin.take().expect("piped");
        let stdout = BufReader::new(child.stdout.take().expect("piped"));
        let mut session = Self {
            child,
            stdin,
            stdout,
        };
        let root = session.solve(req)?;
        Ok((session, root))
    }

    /// Solve another spot on the same serve process, replacing the held game
    /// (P9 analyze scores many spots without respawning the solver).
    pub fn solve(&mut self, req: &SolveRequest) -> io::Result<TreeNode> {
        self.request(json!({"v": PROTOCOL_V, "op": "solve", "config": req}))
    }

    /// Current node's payload.
    pub fn node(&mut self) -> io::Result<TreeNode> {
        self.request(json!({"op": "node"}))
    }

    /// Descend by action index (player nodes).
    pub fn play(&mut self, action: usize) -> io::Result<TreeNode> {
        self.request(json!({"op": "play", "action": action}))
    }

    /// Descend a chance node by dealing `card` (e.g. `"7h"`).
    pub fn deal(&mut self, card: &str) -> io::Result<TreeNode> {
        self.request(json!({"op": "deal", "card": card}))
    }

    /// One step up (no-op at the root).
    pub fn back(&mut self) -> io::Result<TreeNode> {
        self.request(json!({"op": "back"}))
    }

    /// Back to the root.
    pub fn root(&mut self) -> io::Result<TreeNode> {
        self.request(json!({"op": "root"}))
    }

    /// Lock the current player node's strategy (P10, design doc 06). `strategy`
    /// is `[action][hand]` parallel to the node's `freqs`; a hand whose actions
    /// are all `0.0` is left free for the re-solve. Locks accumulate across
    /// nodes and take effect on the next [`resolve`](Self::resolve).
    pub fn lock(&mut self, strategy: &[Vec<f32>]) -> io::Result<TreeNode> {
        self.request(json!({"op": "lock", "strategy": strategy}))
    }

    /// Re-solve holding every lock, returning the node at the current position.
    /// As costly as a fresh solve (seconds-to-minutes), reported on stderr.
    pub fn resolve(&mut self) -> io::Result<TreeNode> {
        self.request(json!({"op": "resolve"}))
    }

    /// At a chance node: per dealable card, the next node's aggregate mix + EV.
    pub fn runouts(&mut self) -> io::Result<Vec<RunoutSummary>> {
        let v = self.round_trip(json!({"op": "runouts"}))?;
        serde_json::from_value(v["runouts"].clone())
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
    }

    /// One request/response round trip.
    fn request(&mut self, req: serde_json::Value) -> io::Result<TreeNode> {
        let v = self.round_trip(req)?;
        serde_json::from_value(v).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
    }

    fn round_trip(&mut self, req: serde_json::Value) -> io::Result<serde_json::Value> {
        writeln!(self.stdin, "{req}")?;
        self.stdin.flush()?;
        let mut line = String::new();
        if self.stdout.read_line(&mut line)? == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "solve-gen serve exited — see its stderr above",
            ));
        }
        parse_response(&line)
    }
}

impl Drop for TreeSession {
    fn drop(&mut self) {
        let _ = writeln!(self.stdin, "{}", json!({"op": "quit"}));
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Parse one response line into JSON, mapping `{"error": …}` to an error.
fn parse_response(line: &str) -> io::Result<serde_json::Value> {
    let v: serde_json::Value =
        serde_json::from_str(line).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    if let Some(msg) = v.get("error").and_then(|m| m.as_str()) {
        return Err(io::Error::new(io::ErrorKind::InvalidData, msg.to_string()));
    }
    Ok(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_node() -> TreeNode {
        TreeNode {
            player: "ip".into(),
            board: vec!["Td".into(), "9d".into(), "6h".into()],
            pot_bb: 6.0,
            line: vec!["Check".into()],
            actions: vec!["Check".into(), "Bet 2.0bb".into()],
            hands: vec!["AsKs".into(), "AhKh".into()],
            freqs: vec![vec![0.2, 0.4], vec![0.8, 0.6]],
            evs: vec![vec![1.0, 1.1], vec![3.5, 3.6]],
            ..Default::default()
        }
    }

    #[test]
    fn to_spot_reshapes_action_major_into_per_hand() {
        let spot = sample_node().to_spot();
        assert_eq!(spot.label, "Check");
        assert_eq!(spot.villain_action, "Check");
        assert!(!spot.hero_oop);
        assert_eq!(spot.strategies.len(), 2);
        let s = &spot.strategies[1];
        assert_eq!(s.hand, "AhKh");
        assert_eq!(s.strategy.frequencies, vec![0.4, 0.6]);
        assert_eq!(s.strategy.action_ev, vec![1.1, 3.6]);
    }

    #[test]
    fn to_spot_at_root_and_at_chance() {
        let mut node = sample_node();
        node.line.clear();
        assert_eq!(node.to_spot().label, "(root)");

        let chance = TreeNode {
            player: "chance".into(),
            dealable: vec!["2c".into()],
            ..Default::default()
        };
        assert!(chance.to_spot().strategies.is_empty());
    }

    #[test]
    fn parse_response_maps_errors_and_tolerates_new_fields() {
        assert_eq!(
            parse_response("{\"error\": \"no game\"}")
                .unwrap_err()
                .to_string(),
            "no game"
        );
        // Unknown fields (a future serve) and absent optional fields must parse.
        let node: TreeNode = serde_json::from_value(
            parse_response(
                "{\"player\":\"chance\",\"board\":[],\"pot_bb\":6.0,\"line\":[],\"actions\":[],\
                 \"dealable\":[\"2c\"],\"weights\":[0.5],\"someday\":1}",
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(node.dealable, vec!["2c"]);
        assert_eq!(node.weights, vec![0.5]);
        assert!(node.hands.is_empty());
    }

    #[test]
    fn runouts_response_parses() {
        let v = parse_response(
            "{\"runouts\":[{\"card\":\"2c\",\"actions\":[\"Check\",\"Bet 5.0bb\"],\
             \"freqs\":[0.6,0.4],\"ev_bb\":1.2}]}",
        )
        .unwrap();
        let runouts: Vec<RunoutSummary> = serde_json::from_value(v["runouts"].clone()).unwrap();
        assert_eq!(runouts.len(), 1);
        assert_eq!(runouts[0].card, "2c");
        assert_eq!(runouts[0].freqs, vec![0.6, 0.4]);
        assert!((runouts[0].ev_bb - 1.2).abs() < 1e-6);
    }

    /// End-to-end: solve a tiny spot through `solve-gen serve` and walk the
    /// tree. Spawns cargo + a real (fast) solve, so it's ignored by default:
    /// `cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn tree_session_walks_a_tiny_solve() {
        let req = SolveRequest {
            flop: "Td9d6h".into(),
            config: crate::solution::SpotConfig {
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
            },
        };
        let (mut s, root) = TreeSession::start(&req).unwrap();
        assert_eq!(root.player, "oop");
        assert_eq!(root.board, vec!["6h", "9d", "Td"]); // solver-sorted
        assert!(root.line.is_empty());
        // At the root every combo reaches, so raw weights are all positive.
        assert_eq!(root.weights.len(), root.hands.len());
        assert!(root.weights.iter().all(|&w| w > 0.0));

        let check = root.actions.iter().position(|a| a == "Check").unwrap();
        let node = s.play(check).unwrap();
        assert_eq!(node.player, "ip");
        assert_eq!(node.line, vec!["Check"]);
        assert_eq!(node.freqs.len(), node.actions.len());
        assert_eq!(node.freqs[0].len(), node.hands.len());
        // P7: payloads carry per-hand reach weights and equity.
        assert_eq!(node.weights.len(), node.hands.len());
        assert_eq!(node.equity.len(), node.hands.len());
        assert!(node.equity.iter().all(|&e| (0.0..=1.0).contains(&e)));

        // Check through to the turn: a chance node with unblocked cards only.
        let check = node.actions.iter().position(|a| a == "Check").unwrap();
        let chance = s.play(check).unwrap();
        assert_eq!(chance.player, "chance");
        assert!(!chance.dealable.contains(&"Td".to_string()));
        assert!(chance.dealable.contains(&"2c".to_string()));

        // P7: runouts summarizes every dealable card without moving the node.
        let runouts = s.runouts().unwrap();
        assert_eq!(runouts.len(), chance.dealable.len());
        let r = runouts.iter().find(|r| r.card == "2c").unwrap();
        assert_eq!(r.actions.len(), r.freqs.len());
        assert!((r.freqs.iter().sum::<f32>() - 1.0).abs() < 1e-3);
        assert_eq!(s.node().unwrap().player, "chance");

        let turn = s.deal("2c").unwrap();
        assert_eq!(turn.board.last().unwrap(), "2c");
        assert_eq!(turn.line.last().unwrap(), "deal 2c");

        let back = s.back().unwrap();
        assert_eq!(back.player, "chance");
        let root2 = s.root().unwrap();
        assert!(root2.line.is_empty());
        assert_eq!(root2.player, "oop");

        // P10: lock the root (OOP) to always-check, re-solve, and confirm the
        // forced strategy took. `strategy` is [action][hand]; the Check row is
        // all 1.0, every other action 0.0.
        let root = s.root().unwrap();
        let check = root.actions.iter().position(|a| a == "Check").unwrap();
        let n = root.hands.len();
        let strategy: Vec<Vec<f32>> = (0..root.actions.len())
            .map(|a| vec![if a == check { 1.0 } else { 0.0 }; n])
            .collect();
        s.lock(&strategy).unwrap();
        let resolved = s.resolve().unwrap();
        assert_eq!(resolved.player, "oop");
        assert!(resolved.freqs[check]
            .iter()
            .all(|&f| (f - 1.0).abs() < 1e-2));

        // Errors are protocol errors, not a dead child.
        assert!(s.play(99).is_err());
        assert!(s.node().is_ok());
    }
}
