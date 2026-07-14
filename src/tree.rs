//! Tree sessions: query arbitrary nodes of a solved game (design doc 01, P4).
//!
//! A [`TreeSession`] holds a long-lived `solve-gen serve` subprocess with the
//! solved game resident in memory, and navigates it over line-delimited JSON on
//! stdio. The [`TreeNode`] payload defined here — not the solver's own tree
//! format — is the wire format (protocol v2, see [`PROTOCOL_V`]), which keeps
//! the AGPL solver behind a process boundary exactly like the snapshot path.

use crate::iso::{translate_node, SuitPerm};
use crate::postflop_table::PostflopTable;
use crate::solution::{solve_gen_command, SolveRequest};
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

/// Read-only navigation over a solved tree, so `drill hand` and the `table`
/// browser drive a live [`TreeSession`] and a disk-backed [`TableWalk`] through
/// the same code. lock/resolve aren't here — a static table can't re-solve;
/// [`live`](TreeWalk::live) hands out the live session for those.
pub trait TreeWalk {
    fn root(&mut self) -> io::Result<TreeNode>;
    fn node(&mut self) -> io::Result<TreeNode>;
    fn play(&mut self, action: usize) -> io::Result<TreeNode>;
    fn deal(&mut self, card: &str) -> io::Result<TreeNode>;
    fn back(&mut self) -> io::Result<TreeNode>;
    fn runouts(&mut self) -> io::Result<Vec<RunoutSummary>>;
    /// The child under action `action` without moving the walker, or `None`
    /// when answering would cost a solve: a live session plays and backs out
    /// (two cheap protocol calls); a table-backed walk answers from the table
    /// only, so passive lenses never trigger the go-live fallback.
    fn peek(&mut self, action: usize) -> io::Result<Option<TreeNode>> {
        let node = self.play(action)?;
        self.back()?;
        Ok(Some(node))
    }
    /// A live, re-solvable session positioned at the current node — spawning
    /// and replaying the line if this walker was disk-backed. lock/resolve and
    /// (from a table) runouts go through here.
    fn live(&mut self) -> io::Result<&mut TreeSession>;
}

// The inherent methods already match the trait; call them fully-qualified so
// these don't recurse into themselves.
impl TreeWalk for TreeSession {
    fn root(&mut self) -> io::Result<TreeNode> {
        TreeSession::root(self)
    }
    fn node(&mut self) -> io::Result<TreeNode> {
        TreeSession::node(self)
    }
    fn play(&mut self, action: usize) -> io::Result<TreeNode> {
        TreeSession::play(self, action)
    }
    fn deal(&mut self, card: &str) -> io::Result<TreeNode> {
        TreeSession::deal(self, card)
    }
    fn back(&mut self) -> io::Result<TreeNode> {
        TreeSession::back(self)
    }
    fn runouts(&mut self) -> io::Result<Vec<RunoutSummary>> {
        TreeSession::runouts(self)
    }
    fn live(&mut self) -> io::Result<&mut TreeSession> {
        Ok(self)
    }
}

/// A disk-backed [`TreeWalk`]: serves reached nodes from a [`PostflopTable`]
/// and, on the first miss — a pruned/rare line, past the turn cap, or a
/// lock/resolve/runouts request — spawns a [`TreeSession`], replays the line,
/// and stays live thereafter. That spawn is the only place the table path
/// touches the solver, so the process-boundary / no-link invariant holds.
pub struct TableWalk {
    table: PostflopTable,
    req: SolveRequest,
    /// User→stored suit map when the table was stored under a suit-isomorphic
    /// flop (design doc 08). `None` = exact hit, serve nodes untranslated.
    perm: Option<SuitPerm>,
    /// The action line to the current node, in the table's stored suit space
    /// (kept only while on the table path).
    line: Vec<String>,
    /// Spawned on the first miss; once live, every op delegates to it (the
    /// table is stale after a re-solve, so there's no going back).
    live: Option<TreeSession>,
}

impl TableWalk {
    /// Build a walker positioned at the table's root node. `perm` is the
    /// user→stored suit map for a table found via suit isomorphism (`None`
    /// for an exact hit).
    pub fn new(
        table: PostflopTable,
        req: SolveRequest,
        perm: Option<SuitPerm>,
    ) -> io::Result<(Self, TreeNode)> {
        let walk = Self {
            table,
            req,
            perm,
            line: Vec::new(),
            live: None,
        };
        let root = walk
            .table
            .node(&[])
            .map(|t| walk.translate(t.node.clone()))
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "table has no root node"))?;
        Ok((walk, root))
    }

    /// A stored node, mapped into the user's suit space when serving via an
    /// isomorph. Action labels are card-free, so line keys need no mapping.
    fn translate(&self, node: TreeNode) -> TreeNode {
        match &self.perm {
            Some(p) => translate_node(node, p),
            None => node,
        }
    }

    /// A user-entered card in the table's stored suit space (the direction of
    /// `deal` keys). Unparseable input passes through to fail like any miss.
    fn to_stored(&self, card: &str) -> String {
        self.perm
            .as_ref()
            .and_then(|p| p.card(card))
            .unwrap_or_else(|| card.to_string())
    }

    /// The current node from the table (only reached while `live` is `None`, so
    /// the line is always a stored node).
    fn lookup(&self) -> io::Result<TreeNode> {
        self.table
            .node(&self.line)
            .map(|t| self.translate(t.node.clone()))
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::NotFound,
                    format!("node not in table: {:?}", self.line),
                )
            })
    }

    /// Ensure a live session exists at `self.line`, spawning and replaying the
    /// line from the root exactly once. Prints the solve on stderr like any
    /// other live solve. The live solve is of the user's raw flop, so stored
    /// deal labels map back through the perm during replay; action labels are
    /// identical in both suit spaces.
    fn go_live(&mut self) -> io::Result<&mut TreeSession> {
        if self.live.is_none() {
            let (session, root) = TreeSession::start(&self.req)?;
            self.live = Some(session);
            let s = self.live.as_mut().unwrap();
            let back = self.perm.as_ref().map(SuitPerm::inverse);
            let mut node = root;
            for label in self.line.clone() {
                node = if let Some(card) = label.strip_prefix("deal ") {
                    let card = back
                        .as_ref()
                        .and_then(|p| p.card(card))
                        .unwrap_or_else(|| card.to_string());
                    s.deal(&card)?
                } else {
                    let i = node
                        .actions
                        .iter()
                        .position(|a| a == &label)
                        .ok_or_else(|| {
                            io::Error::new(
                                io::ErrorKind::InvalidData,
                                format!("replay: no action {label:?} at {:?}", node.line),
                            )
                        })?;
                    s.play(i)?
                };
            }
        }
        Ok(self.live.as_mut().unwrap())
    }
}

impl TreeWalk for TableWalk {
    fn root(&mut self) -> io::Result<TreeNode> {
        self.line.clear();
        match &mut self.live {
            Some(s) => s.root(),
            None => self.lookup(),
        }
    }

    fn node(&mut self) -> io::Result<TreeNode> {
        match &mut self.live {
            Some(s) => s.node(),
            None => self.lookup(),
        }
    }

    fn play(&mut self, action: usize) -> io::Result<TreeNode> {
        if let Some(s) = &mut self.live {
            return s.play(action);
        }
        // The child's label is the current (stored) node's action label — the
        // exact string the generator keyed the child on (card-free, so it is
        // the same in both suit spaces).
        let cur = self.lookup()?;
        let label = cur.actions.get(action).cloned().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("action {action} out of range at {:?}", cur.line),
            )
        })?;
        self.line.push(label);
        match self.table.node(&self.line) {
            Some(t) => Ok(self.translate(t.node.clone())),
            None => self.go_live().and_then(TreeSession::node),
        }
    }

    fn deal(&mut self, card: &str) -> io::Result<TreeNode> {
        if let Some(s) = &mut self.live {
            return s.deal(card);
        }
        let stored = self.to_stored(card);
        self.line.push(format!("deal {stored}"));
        match self.table.node(&self.line) {
            Some(t) => Ok(self.translate(t.node.clone())),
            None => self.go_live().and_then(TreeSession::node),
        }
    }

    fn back(&mut self) -> io::Result<TreeNode> {
        if let Some(s) = &mut self.live {
            return s.back();
        }
        self.line.pop();
        self.lookup()
    }

    fn runouts(&mut self) -> io::Result<Vec<RunoutSummary>> {
        // ponytail: runouts always live-solve from a table (browser-only op);
        // synthesize from the stored turn children if this spawn ever bites.
        self.go_live()?.runouts()
    }

    fn peek(&mut self, action: usize) -> io::Result<Option<TreeNode>> {
        if let Some(s) = &mut self.live {
            let node = s.play(action)?;
            s.back()?;
            return Ok(Some(node));
        }
        let cur = self.lookup()?;
        let Some(label) = cur.actions.get(action) else {
            return Ok(None);
        };
        let mut child = self.line.clone();
        child.push(label.clone());
        Ok(self
            .table
            .node(&child)
            .map(|t| self.translate(t.node.clone())))
    }

    fn live(&mut self) -> io::Result<&mut TreeSession> {
        self.go_live()
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

    /// End-to-end: generate a reach-pruned table for a small spot, walk it, and
    /// check it against a fresh live solve — the generator's reach/cap walk plus
    /// the [`TableWalk`] fallback. Shells out to `solve-gen tables` and spawns a
    /// live serve, so it's ignored: `cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn table_walk_matches_a_live_solve_and_caps_at_the_turn() {
        // Symmetric ranges so neither side dominates: the equilibrium mixes and
        // reaches the turn. (Asymmetric toy ranges fold out on the flop, which
        // correctly prunes to a handful of nodes — no turns to test.)
        let range = "99+,AJs+,KQs,AQo+";
        let config = crate::solution::SpotConfig {
            formation: "srp-btn-bb".into(),
            oop_range: range.into(),
            ip_range: range.into(),
            flop_sizes: "50%".into(),
            turn_sizes: "33%".into(),
            river_sizes: "33%".into(),
            stack_bb: 97.0,
            pot_bb: 6.0,
            rake_rate: 0.0,
            rake_cap_bb: 0.0,
        };
        let req = SolveRequest {
            flop: "Td9d6h".into(),
            config: config.clone(),
        };
        let out = std::env::temp_dir().join(format!("pt-table-gen-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&out);

        // Generate: threshold low enough that even rare check lines are stored.
        let args: Vec<String> = [
            "tables",
            "--flop",
            "Td9d6h",
            "--formation",
            "srp-btn-bb",
            "--oop",
            range,
            "--ip",
            range,
            "--sizes",
            "50%",
            "--turn-sizes",
            "33%",
            "--river-sizes",
            "33%",
            "--reach",
            "0.0001",
        ]
        .iter()
        .map(|s| s.to_string())
        .chain([String::from("--out"), out.to_string_lossy().into_owned()])
        .collect();
        let status = crate::solution::solve_gen_command(&args).status().unwrap();
        assert!(status.success(), "solve-gen tables failed");

        let dir = out.join("srp-btn-bb");
        let table = PostflopTable::load(&dir, "Td9d6h", &config.hash8()).unwrap();

        // Structural: root reaches with probability 1; turn decision nodes are
        // stored, but the turn→river chance node and anything past it are not.
        assert_eq!(table.node(&[]).unwrap().reach, 1.0);
        assert!(
            table
                .nodes()
                .any(|t| t.node.board.len() == 4 && matches!(t.node.player.as_str(), "oop" | "ip")),
            "turn decision nodes must be stored"
        );
        assert!(
            !table
                .nodes()
                .any(|t| t.node.player == "chance" && t.node.board.len() >= 4),
            "the turn→river chance node must be capped out"
        );
        assert!(
            !table.nodes().any(|t| t.node.board.len() >= 5),
            "nothing past the turn is stored"
        );

        let close = |a: &[Vec<f32>], b: &[Vec<f32>]| {
            a.len() == b.len()
                && a.iter().zip(b).all(|(x, y)| {
                    x.len() == y.len() && x.iter().zip(y).all(|(p, q)| (p - q).abs() <= 0.02)
                })
        };

        let (mut walk, root) = TableWalk::new(table, req.clone(), None).unwrap();
        let (mut live, live_root) = TreeSession::start(&req).unwrap();
        assert_eq!(root.player, "oop");
        assert_eq!(root.hands, live_root.hands);
        assert!(
            close(&root.freqs, &live_root.freqs),
            "root strategy matches"
        );

        // A stored flop line: OOP check → IP. Table freqs match the live solve.
        let ci = root.actions.iter().position(|a| a == "Check").unwrap();
        let ip = walk.play(ci).unwrap();
        let ip_live = live.play(ci).unwrap();
        assert_eq!(ip.player, "ip");
        assert!(close(&ip.freqs, &ip_live.freqs), "flop line matches");

        // A stored turn line: IP check → turn chance → deal a card → turn node.
        let ci2 = ip.actions.iter().position(|a| a == "Check").unwrap();
        let chance = walk.play(ci2).unwrap();
        assert_eq!(chance.player, "chance");
        assert_eq!(chance.board.len(), 3);
        let card = chance.dealable[0].clone();
        let turn = walk.deal(&card).unwrap();
        live.play(ci2).unwrap();
        let turn_live = live.deal(&card).unwrap();
        assert_eq!(turn.board.len(), 4);
        assert!(close(&turn.freqs, &turn_live.freqs), "turn line matches");

        // Past the cap: check the turn through to the river chance node. That
        // line isn't stored, so the walk transparently falls back to a live
        // solve — no panic, and it lands on the river-dealing chance node.
        let mut node = turn;
        for _ in 0..3 {
            if node.player == "chance" && node.board.len() == 4 {
                break;
            }
            let c = node.actions.iter().position(|a| a == "Check").unwrap();
            node = walk.play(c).unwrap();
        }
        assert_eq!(node.player, "chance");
        assert_eq!(node.board.len(), 4, "fell back onto the river chance node");

        let _ = std::fs::remove_dir_all(&out);
    }

    use crate::postflop_table::{TableHeader, TableNode, FORMAT_VERSION};
    use crate::solution::{GenInfo, SpotConfig};

    fn tiny_config() -> SpotConfig {
        SpotConfig {
            formation: "srp-btn-bb".into(),
            oop_range: "22".into(),
            ip_range: "33".into(),
            flop_sizes: "50%".into(),
            turn_sizes: "33%".into(),
            river_sizes: "33%".into(),
            stack_bb: 97.0,
            pot_bb: 6.0,
            rake_rate: 0.0,
            rake_cap_bb: 0.0,
        }
    }

    /// On-disk fixture mirroring `postflop_table`'s test helper.
    fn write_fixture(dir: &std::path::Path, flop: &str, config: &SpotConfig, nodes: &[TableNode]) {
        std::fs::create_dir_all(dir).unwrap();
        let header = TableHeader {
            version: FORMAT_VERSION,
            formation: config.formation.clone(),
            config: config.clone(),
            config_hash: config.hash8(),
            generator: GenInfo {
                version: "0.1.0".into(),
                exploitability_bb: 0.02,
            },
            reach: 0.002,
        };
        std::fs::write(
            dir.join(format!("header-{}.json", header.config_hash)),
            serde_json::to_string_pretty(&header).unwrap(),
        )
        .unwrap();
        let lines: Vec<String> = nodes
            .iter()
            .map(|n| serde_json::to_string(n).unwrap())
            .collect();
        std::fs::write(
            dir.join(format!(
                "{}-{}.jsonl",
                flop.to_lowercase(),
                header.config_hash
            )),
            lines.join("\n"),
        )
        .unwrap();
    }

    fn table_node(reach: f32, node: TreeNode) -> TableNode {
        TableNode { reach, node }
    }

    /// Stored space Td9d6h served to a user who typed Ts9s6h: every boundary
    /// of the walk translates (outbound payloads, inbound deals, peeks), the
    /// parallel per-hand arrays re-sort in lockstep, and a passive peek at an
    /// unstored child answers `None` instead of going live.
    #[test]
    fn iso_table_walk_translates_between_suit_spaces() {
        let config = tiny_config();
        let dir = std::env::temp_dir().join(format!("pt-iso-walk-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);

        let root = table_node(
            1.0,
            TreeNode {
                player: "oop".into(),
                board: vec!["6h".into(), "9d".into(), "Td".into()],
                pot_bb: 6.0,
                line: vec![],
                actions: vec!["Check".into(), "Bet 2.0bb".into()],
                hands: vec!["2d2c".into(), "2s2c".into()],
                freqs: vec![vec![0.6, 0.4], vec![0.4, 0.6]],
                evs: vec![vec![1.0, 2.0], vec![3.0, 4.0]],
                weights: vec![0.1, 0.2],
                equity: vec![0.5, 0.7],
                ..Default::default()
            },
        );
        let chance = table_node(
            0.6,
            TreeNode {
                player: "chance".into(),
                board: vec!["6h".into(), "9d".into(), "Td".into()],
                pot_bb: 6.0,
                line: vec!["Check".into()],
                dealable: vec!["2c".into(), "2d".into()],
                ..Default::default()
            },
        );
        let turn = table_node(
            0.6,
            TreeNode {
                player: "oop".into(),
                board: vec!["6h".into(), "9d".into(), "Td".into(), "2d".into()],
                pot_bb: 6.0,
                line: vec!["Check".into(), "deal 2d".into()],
                actions: vec!["Check".into()],
                hands: vec!["2s2c".into()],
                freqs: vec![vec![1.0]],
                evs: vec![vec![1.5]],
                weights: vec![1.0],
                equity: vec![0.5],
                ..Default::default()
            },
        );
        write_fixture(&dir, "Td9d6h", &config, &[root, chance, turn]);
        let table = PostflopTable::load(&dir, "Td9d6h", &config.hash8()).unwrap();

        // The composed user→stored map, exactly as the lookup derives it.
        let (canon_user, to_canon_user) = crate::iso::canonical_flop("Ts9s6h").unwrap();
        let (canon_file, to_canon_file) = crate::iso::canonical_flop("Td9d6h").unwrap();
        assert_eq!(canon_user, canon_file, "same isomorphism class");
        let q = to_canon_file.inverse().compose(&to_canon_user);

        let req = SolveRequest {
            flop: "Ts9s6h".into(),
            config,
        };
        let (mut walk, root) = TableWalk::new(table, req, Some(q)).unwrap();

        // Outbound: user-space board, hands re-sorted with arrays in lockstep
        // (stored "2d2c"→user 2s2c has the bigger key, so the order flips).
        assert_eq!(root.board, vec!["6h", "9s", "Ts"]);
        assert_eq!(root.hands, vec!["2d2c", "2s2c"]);
        assert_eq!(root.weights, vec![0.2, 0.1]);
        assert_eq!(root.freqs, vec![vec![0.4, 0.6], vec![0.6, 0.4]]);

        // Passive peek: stored child answers translated, unstored answers
        // None — and never spawns a live session.
        let peeked = walk.peek(0).unwrap().unwrap();
        assert_eq!(peeked.dealable, vec!["2c", "2s"]);
        assert!(walk.peek(1).unwrap().is_none(), "pruned child peeks None");
        assert_eq!(walk.node().unwrap().line, Vec::<String>::new());

        // Inbound: a user-space deal reaches the stored child through the perm,
        // and the served line reads back in user space.
        let chance = walk.play(0).unwrap();
        assert_eq!(chance.player, "chance");
        assert_eq!(chance.dealable, vec!["2c", "2s"]);
        let turn = walk.deal("2s").unwrap();
        assert_eq!(turn.board, vec!["6h", "9s", "Ts", "2s"]);
        assert_eq!(turn.line, vec!["Check", "deal 2s"]);
        // Stored 2s2c maps to user 2d2c — consistently unblocked by the
        // user-space turn card 2s, as the stored hand was by the stored 2d.
        assert_eq!(turn.hands, vec!["2d2c"]);

        let back = walk.back().unwrap();
        assert_eq!(back.player, "chance");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// End-to-end iso equivalence: generate a table for Td9d6h, open it via
    /// the suit-isomorphic Ts9s6h, and compare against a fresh live solve of
    /// Ts9s6h itself — the exactness claim of design doc 08, plus the go-live
    /// replay translation. Spawns solve-gen twice: `cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn iso_table_walk_matches_live_solve_of_raw_flop() {
        let range = "99+,AJs+,KQs,AQo+";
        let config = SpotConfig {
            formation: "srp-btn-bb".into(),
            oop_range: range.into(),
            ip_range: range.into(),
            flop_sizes: "50%".into(),
            turn_sizes: "33%".into(),
            river_sizes: "33%".into(),
            stack_bb: 97.0,
            pot_bb: 6.0,
            rake_rate: 0.0,
            rake_cap_bb: 0.0,
        };
        let out = std::env::temp_dir().join(format!("pt-iso-gen-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&out);
        let args: Vec<String> = [
            "tables",
            "--flop",
            "Td9d6h",
            "--formation",
            "srp-btn-bb",
            "--oop",
            range,
            "--ip",
            range,
            "--sizes",
            "50%",
            "--turn-sizes",
            "33%",
            "--river-sizes",
            "33%",
            "--reach",
            "0.0001",
        ]
        .iter()
        .map(|s| s.to_string())
        .chain([String::from("--out"), out.to_string_lossy().into_owned()])
        .collect();
        let status = crate::solution::solve_gen_command(&args).status().unwrap();
        assert!(status.success(), "solve-gen tables failed");

        let table = PostflopTable::load(out.join("srp-btn-bb"), "Td9d6h", &config.hash8()).unwrap();
        let (_, to_canon_user) = crate::iso::canonical_flop("Ts9s6h").unwrap();
        let (_, to_canon_file) = crate::iso::canonical_flop("Td9d6h").unwrap();
        let q = to_canon_file.inverse().compose(&to_canon_user);
        let req = SolveRequest {
            flop: "Ts9s6h".into(),
            config,
        };

        let (mut walk, root) = TableWalk::new(table, req.clone(), Some(q)).unwrap();
        let (mut live, live_root) = TreeSession::start(&req).unwrap();

        let close = |a: &[Vec<f32>], b: &[Vec<f32>]| {
            a.len() == b.len()
                && a.iter().zip(b).all(|(x, y)| {
                    x.len() == y.len() && x.iter().zip(y).all(|(p, q)| (p - q).abs() <= 0.02)
                })
        };

        // The translated table must be element-for-element parallel to the
        // live solve of the raw flop: same board, same hands in the same
        // order, matching strategy.
        assert_eq!(root.board, live_root.board);
        assert_eq!(root.hands, live_root.hands, "hand order re-sorts exactly");
        assert!(close(&root.freqs, &live_root.freqs), "root strategy");

        let ci = root.actions.iter().position(|a| a == "Check").unwrap();
        let ip = walk.play(ci).unwrap();
        let ip_live = live.play(ci).unwrap();
        assert_eq!(ip.hands, ip_live.hands);
        assert!(close(&ip.freqs, &ip_live.freqs), "flop line");

        let ci2 = ip.actions.iter().position(|a| a == "Check").unwrap();
        let chance = walk.play(ci2).unwrap();
        let chance_live = live.play(ci2).unwrap();
        assert_eq!(chance.player, "chance");
        assert_eq!(chance.dealable, chance_live.dealable, "user-space runouts");

        let card = chance.dealable[0].clone();
        let turn = walk.deal(&card).unwrap();
        let turn_live = live.deal(&card).unwrap();
        assert_eq!(turn.board, turn_live.board);
        assert_eq!(turn.hands, turn_live.hands);
        assert!(close(&turn.freqs, &turn_live.freqs), "turn line");

        // Off the stored frontier: the walk go-lives on the raw flop,
        // replaying the stored line back through the perm.
        let mut node = turn;
        for _ in 0..3 {
            if node.player == "chance" && node.board.len() == 4 {
                break;
            }
            let c = node.actions.iter().position(|a| a == "Check").unwrap();
            node = walk.play(c).unwrap();
        }
        assert_eq!(node.player, "chance");
        assert_eq!(node.board.len(), 4, "replayed onto the river chance node");
        assert_eq!(&node.board[..3], &["6h", "9s", "Ts"], "raw-flop live board");

        let _ = std::fs::remove_dir_all(&out);
    }
}
