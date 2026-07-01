//! Tree sessions: query arbitrary nodes of a solved game (design doc 01, P4).
//!
//! A [`TreeSession`] holds a long-lived `solve-gen serve` subprocess with the
//! solved game resident in memory, and navigates it over line-delimited JSON on
//! stdio. The [`TreeNode`] payload defined here — not the solver's own tree
//! format — is the wire format (protocol v1), which keeps the AGPL solver
//! behind a process boundary exactly like the snapshot path.

use crate::solution::{solve_gen_command, HandStrategy, NodeStrategy, SolveRequest, SolvedSpot};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::io::{self, BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Stdio};

/// Protocol version sent with `op:solve`; serve rejects versions it can't speak.
pub const PROTOCOL_V: u32 = 1;

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
        let root = session.request(json!({"v": PROTOCOL_V, "op": "solve", "config": req}))?;
        Ok((session, root))
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

    /// One request/response round trip.
    fn request(&mut self, req: serde_json::Value) -> io::Result<TreeNode> {
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

/// Parse one response line: a `TreeNode`, or `{"error": …}` mapped to an error.
fn parse_response(line: &str) -> io::Result<TreeNode> {
    let v: serde_json::Value =
        serde_json::from_str(line).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    if let Some(msg) = v.get("error").and_then(|m| m.as_str()) {
        return Err(io::Error::new(io::ErrorKind::InvalidData, msg.to_string()));
    }
    serde_json::from_value(v).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
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
        // Unknown fields (a v2 serve) and absent optional fields must parse.
        let node = parse_response(
            "{\"player\":\"chance\",\"board\":[],\"pot_bb\":6.0,\"line\":[],\"actions\":[],\
             \"dealable\":[\"2c\"],\"weights\":[0.5]}",
        )
        .unwrap();
        assert_eq!(node.dealable, vec!["2c"]);
        assert!(node.hands.is_empty());
    }

    /// End-to-end: solve a tiny spot through `solve-gen serve` and walk the
    /// tree. Spawns cargo + a real (fast) solve, so it's ignored by default:
    /// `cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn tree_session_walks_a_tiny_solve() {
        let mut req = SolveRequest::new("Td9d6h");
        req.oop = Some("AA,KK".into());
        req.ip = Some("QQ,JJ".into());
        req.sizes = Some("50%".into());
        let (mut s, root) = TreeSession::start(&req).unwrap();
        assert_eq!(root.player, "oop");
        assert_eq!(root.board, vec!["6h", "9d", "Td"]); // solver-sorted
        assert!(root.line.is_empty());

        let check = root.actions.iter().position(|a| a == "Check").unwrap();
        let node = s.play(check).unwrap();
        assert_eq!(node.player, "ip");
        assert_eq!(node.line, vec!["Check"]);
        assert_eq!(node.freqs.len(), node.actions.len());
        assert_eq!(node.freqs[0].len(), node.hands.len());

        // Check through to the turn: a chance node with unblocked cards only.
        let check = node.actions.iter().position(|a| a == "Check").unwrap();
        let chance = s.play(check).unwrap();
        assert_eq!(chance.player, "chance");
        assert!(!chance.dealable.contains(&"Td".to_string()));
        assert!(chance.dealable.contains(&"2c".to_string()));

        let turn = s.deal("2c").unwrap();
        assert_eq!(turn.board.last().unwrap(), "2c");
        assert_eq!(turn.line.last().unwrap(), "deal 2c");

        let back = s.back().unwrap();
        assert_eq!(back.player, "chance");
        let root2 = s.root().unwrap();
        assert!(root2.line.is_empty());
        assert_eq!(root2.player, "oop");

        // Errors are protocol errors, not a dead child.
        assert!(s.play(99).is_err());
        assert!(s.node().is_ok());
    }
}
