# Design 01 — Tree sessions: `solve-gen serve` (P4)

**Unlocks:** tree browsing (03), multi-street drills (04), analyze (05),
nodelocking (06). **Depends on:** nothing.

## Problem

A solve produces a full game tree; today `solve-gen` flattens it into three
`SolvedSpot` snapshots and exits. Every parity feature needs arbitrary-node
access. Full-tree export is 50–500 MB/flop (Pio-scale) — wrong for a local
tool. Keep the solved game **resident in the solver process** and query it.

## Shape

`solve-gen serve` — a long-lived subprocess speaking **line-delimited JSON
over stdio**. The trainer spawns it exactly like today's shell-out (same
`POKER_TRAINER_SOLVE_GEN` / `cargo run -p solve-gen` fallback), so the AGPL
boundary stays a process boundary.

Why stdio JSON: zero new dependencies (`serde_json` is on both sides), no
ports/sockets, trivially debuggable (`echo '{"op":"node",...}' | solve-gen
serve …`), and it keeps invariant #1 by construction.

### Protocol (v2)

One JSON object per line each way. Every response echoes `"id"` if given.
Errors: `{"error": "message"}`. (v1 was the pre-P6 shape whose solve body
carried sparse overrides instead of a full `SpotConfig`; serve rejects it.)

```jsonc
// trainer -> solver
{"v":2, "op":"solve", "config": {flop, SpotConfig}}   // solve (or load cached), hold in memory
{"op":"node"}                                       // payload for the current node
{"op":"play", "action": 2}                          // descend by action index
{"op":"deal", "card": "7h"}                         // descend a chance node
{"op":"back"}                                       // one step up
{"op":"root"}                                       // back to root
{"op":"runouts"}                                    // at a chance node: summary per card
{"op":"lock", "strategy": [ ... ]}                  // P10, see doc 06
{"op":"resolve"}                                    // P10: re-solve after locks
{"op":"snapshot", "label": "..."}                   // current node as SolvedSpot JSON
{"op":"quit"}

// solver -> trainer: the node payload
{
  "player": "oop" | "ip" | "chance" | "terminal",
  "board": ["Td","9d","6h","2c"],
  "pot_bb": 10.0,
  "line": ["Check", "Bet 2.0bb", "Call", "deal 2c"],   // labels, for display
  "actions": ["Check", "Bet 3.3bb"],                    // empty for chance/terminal
  "dealable": ["2c", "2d", ...],   // chance nodes only: cards that can fall
  "hands": ["2c2d", ...],          // acting player's combos
  "weights": [0.97, ...],          // reach probability (normalized)
  "equity": [0.55, ...],           // vs. villain's reaching range
  "freqs": [[...], [...]],         // [action][hand]
  "evs": [[...], [...]]            // [action][hand], bb
}
```

`weights` and `equity` come from `cache_normalized_weights()` + the solver's
`equity()`; the study views in 03 need them, and they're free to include.
`runouts` returns, per dealable card, the next player's aggregate action
frequencies and EV — exactly what the runouts view (03) renders.

Chance cards are chosen by the *trainer* (uniform over unblocked cards) so
drill dealing logic stays testable; `deal` just navigates.

### Trainer side (`src/solution.rs`)

```rust
pub struct TreeSession { child: Child, stdin: ..., stdout: ... }
impl TreeSession {
    pub fn start(req: &SolveRequest) -> io::Result<(Self, TreeNode)>; // spawn + solve, root node back
    pub fn solve(&mut self, req: &SolveRequest) -> io::Result<TreeNode>; // new spot, same process (05)
    pub fn node(&mut self) -> io::Result<TreeNode>;
    pub fn play(&mut self, action: usize) -> io::Result<TreeNode>;
    pub fn deal(&mut self, card: &str) -> io::Result<TreeNode>;
    pub fn back(&mut self) -> io::Result<TreeNode>;
    pub fn root(&mut self) -> io::Result<TreeNode>;
    pub fn runouts(&mut self) -> io::Result<Vec<RunoutSummary>>;
    pub fn lock(&mut self, strategy: &[Vec<f32>]) -> io::Result<TreeNode>;   // P10
    pub fn resolve(&mut self) -> io::Result<TreeNode>;                        // P10
}
```

Concrete struct, **no trait**: `SolutionProvider` keeps serving the snapshot
drills, and `TreeSession` is the only tree source. Introduce a trait only when
a second tree source exists (e.g. a future on-disk tree cache). Kill the child
on `Drop`; on a dead/garbled child, surface the error and let callers fall
back to snapshots.

### Caching solved games

Key = hash of the canonical `SpotConfig` JSON (also fixes today's bug-shaped
limitation: custom `--board` solves overwrite the curated flop's snapshot
files — see 02). Store solver-native saves via the solver's `bincode` feature
(`save_data_to_file`, verified present at the pinned rev) under
`~/.cache/poker-trainer/solves/<flop>-<hash8>.bin` (the flop isn't part of
`SpotConfig`, so it rides in the filename). Cache files are an **AGPL-side
implementation detail** — the trainer never reads them, so invariant #2 holds.

The bincode-rc pin is sorted: solve-gen pins `bincode =2.0.0-rc.3` **and**
`bincode_derive =2.0.0-rc.3` (both of the solver's `^2.0.0-rc.3` requirements
would otherwise resolve to the API-breaking 2.0 stable releases). The cache is
an optimization, not part of the protocol: a corrupt file re-solves and
overwrites, a failed save only warns.

Loading a cached 100-flop-tree save is seconds vs. minutes to re-solve;
`serve` answers `{"cached": true}` in the solve ack so UIs can set
expectations.

## Memory & lifetime

One serve process ≈ 1 GB RSS (same as today's solve) for its lifetime. One
session per solved config; drills and the study TUI hold one for the whole
run. Never hold two sessions concurrently by default; `analyze` (05) does its
own budgeting.

## Milestones

1. `serve` with `solve`/`node`/`play`/`deal`/`back`/`root`/`quit`; `TreeSession`;
   `table` gains tree walking behind it (03 M1). Snapshot generation moves to
   `op:snapshot` internally; `solve`/`gen` subcommands keep working unchanged.
2. `runouts` op + `weights`/`equity` in payloads.
3. Config-hash save cache (behind the bincode pin).
4. `lock`/`resolve` ops (P10, doc 06).

## Out of scope

- A neutral on-disk full-tree format. Revisit only if serve latency (solve or
  cache-load) proves unacceptable for a workflow that snapshots can't cover.
- Concurrent multi-spot serving in one process (memory-bound; spawn per config).
