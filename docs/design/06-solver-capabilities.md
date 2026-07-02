# Design 06 — Solver capabilities: nodelocking, ICM, multiway, bunching

What the engine under `solve-gen` can and cannot do, verified against the
pinned rev (`9d1509f`) — and what that makes cheap, expensive, or impossible.
Everything here stays AGPL-side; the trainer only ever sees protocol JSON.

## Verified capability table (pinned rev)

| capability | status in `postflop-solver` | consequence |
|---|---|---|
| Node locking | ✅ `lock_current_strategy(&[f32])` | nodelocking is a P10 feature, not a fork |
| Bunching effect | ✅ `set_bunching_effect(BunchingData)` | folded-range realism is a config option |
| Rake | ✅ `rake_rate`/`rake_cap` in `TreeConfig` | plumb through in 02; trivial |
| Game save/load | ✅ `save_data_to_file` (behind `bincode` feature, currently off) | the 01 solve cache |
| Solve accuracy knobs | ✅ target exploitability, iteration cap, 16-bit compression | expose in `SpotConfig`/CLI |
| Preflop trees | ❌ (`BoardState::Flop` is the earliest root) | preflop stays chart-data (02) |
| ICM / custom terminal utility | ❌ chip-EV payoffs only | fork-level work; research |
| >2 players | ❌ two-player engine | out of scope |

## Nodelocking (P10)

The one big commercial feature that is pure integration work for us.

Flow: browse to a villain node in `table` (03) → `L` opens the lock editor —
edit at **grid-cell granularity** (169 cells × actions; the same granularity
GTO Wizard offers), with bucket presets as shortcuts ("overfold ×1.5",
"never raise") → `lock` op expands cell edits to per-combo frequencies →
`resolve` re-solves the tree with the lock held → the grid re-renders with a
**EV-delta lens** vs. the unlocked baseline (the session keeps both).

Protocol (01): `{"op":"lock", "line":[...], "strategy":[per-hand-per-action]}`
+ `{"op":"resolve"}`. Trainer sends absolute frequencies; expansion from
cells to combos happens trainer-side (it owns the grid mapping already).

Saved locks: a named JSON file (lock line + cell edits) alongside the config —
this is also how "exploitative villain personas" for drills (04) exist for
free: drill against a session with a saved lock applied.

Re-solve cost is a fraction of a fresh solve (warm start from the locked
strategy); still seconds-to-minutes, honestly reported.

### Shipped (P10 M1)

The `lock`/`resolve` protocol ops, `TreeSession::lock`/`resolve`, and a
`table` lock-edit mode: `L` toggles it at a player node, `1`–`9` set the
focused cell to a pure action (a "never raise" / "always call" style lock),
`c` clears it, `R` expands the cell edits to per-combo frequencies, sends the
lock, and re-solves. The grid then switches to a `d` EV-delta lens comparing
the re-solved EVs against the pre-lock baseline at that node.

Shipped later (P10 M2): lock-mode presets — `o` overfold ×1.5, `n` never
raise, whole-node cell edits derived from the current mix — and saved lock
files: `S` writes the line + cell edits as JSON (`--locks` path or an
auto-named file), and `table --locks <file>` replays them on startup,
opening on the delta lens.

Deliberately deferred (lazy version shipped, upgrade when wanted):
- **Drill "villain personas"** — `drill hand` taking a `--locks` file so you
  practice against the saved exploit profile (04's reuse).
- **Cross-node EV-delta.** The delta lens is valid at the node you resolved on;
  navigating clears the baseline. A session-wide delta needs `serve` to retain
  the unlocked game alongside the locked one.
- **Warm-start re-solve.** `resolve` re-solves from a fresh allocation, so it
  costs about a full solve rather than a fraction.

## Bunching (config option, low priority)

`BunchingData` conditions the deal on folded players' ranges (e.g. UTG/MP
folds shift BTN-vs-BB cards). Costs real memory/precompute and matters at the
margins; expose as an optional formation field in 02's `SpotConfig` once
someone actually wants it. Not part of any parity phase.

## ICM / MTT postflop — research, honestly labeled

Chip-EV is baked into the engine's terminal-node payoffs; ICM needs a utility
transform there, i.e. a **fork of postflop-solver** maintained in (or beside)
`solve-gen` — AGPL-compatible, but real CFR-internals work plus validation
against known ICM solutions.

Interim stance (what pre-2023 commercial tools did): MTT formations solve as
chip-EV with ICM-aware *preflop* charts (02 data), and every MTT-labeled
output carries a "chip-EV postflop" caveat. The fork happens only if MTT
training becomes this project's actual use case.

## Multiway — out of scope, permanently-until-proven-otherwise

Two-player is an engine assumption, not a flag. Multiway CFR also loses the
Nash guarantee (equilibria are neither unique nor exploitability-bounded),
which is why commercial multiway is approximate NN territory. Revisit only if
a credible open multiway engine appears; nothing in our formats blocks it
(`SpotConfig.formation` and per-node `player` generalize).

## Accuracy & performance knobs

Expose in `SpotConfig` + CLI (defaults unchanged): target exploitability
(today 0.5% of pot), max iterations (1000), memory compression
(`allocate_memory(true)` = 16-bit, roughly halves the ~1 GB RSS for a small
precision cost — the knob for 8 GB laptops). Every snapshot already gets the
achieved exploitability recorded via `GenInfo` (02), so library quality is
auditable.
