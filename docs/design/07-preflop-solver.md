# 07 — Preflop solver: solved 6-max charts (`crates/preflop-gen`)

Status: **M1 shipped** (format seam + game engine + `tree`); M2–M8 land in
order (milestones below). Reverses the "preflop stays chart-data" stance of
[02](02-solution-library.md)/[06](06-solver-capabilities.md) — for preflop
only. The postflop engine's limits are unchanged.

## Why a second generator crate

GTO-Wizard-style preflop charts (6-max, multiple open/3-bet sizes, ICM,
antes) cannot come from `postflop-solver`: it is heads-up, postflop-rooted
(`BoardState::Flop`), chip-EV only. No open-source alternative covers the
requirement, and vendor charts are ToS-restricted and can't express custom
rule sets like Poker Chase anyway. So `crates/preflop-gen` implements the
standard recipe itself: external-sampling MCCFR over the 169 canonical hand
classes on a capped preflop betting tree.

License posture: preflop-gen is **MIT OR Apache-2.0** and must never link
`postflop-solver` (test-enforced once the crate grows deps, mirroring
`trainer_never_links_the_solver`). Unlike solve-gen, no process boundary is
needed — but it stays a separate crate because it's an offline generator,
not trainer code. The chart *format* lives in the trainer (`src/preflop.rs`)
and preflop-gen depends on the trainer, the same direction as solve-gen.

## Rulesets (`manifests/preflop/<id>.toml`)

One TOML per rule set: seats (acting order, blinds always the last two),
uniform effective stack, blinds, per-player dead ante, the raise menus
(`open_to_bb`, `threebet_mult`, `squeeze_mult`, `fourbet_mult`,
`jam_from_level`), rake (`no flop no drop`), optional `icm_payouts` (absent =
chip-EV cash), and `[solver]` knobs (traversals, seed, export/starter reach).
The header written next to every solve echoes the config verbatim and carries
its FNV-1a `config_hash`; `gen` skips rulesets whose hash already matches
(the solve-gen resumability contract).

Shipped rulesets: `cash100` (6-max 100bb, 5% rake capped at 3bb) and the
Poker Chase ladder `poker-chase-{25,40,60}` (0.25bb ante ⇒ 3bb root pot, ICM
payouts 4-2-1-0-0-0, no rake; 25bb offers the jam from vs-open on).

## The betting tree (`game.rs`)

A pure state machine — never materialized. Integer centi-bb pot math. Rules,
each a ruleset knob with a named ceiling:

- **No limps**: unopened action is fold-or-raise; a walk ends the hand before
  the BB ever acts unopened. (Upgrade: a limp token + check-closes-round.)
- Raise ladder: open (menu, absolute bb) → 3-bet (menu × the open; a single
  squeeze size once the open has a caller) → 4-bet (single size × the 3-bet)
  → 5-bet jam-only. All-in joins the menu from `jam_from_level` on; menu
  sizes at/over the stack collapse into the jam. Facing a jam: fold/call.
- Multiway is otherwise uncapped (cold-calls, squeezes, cold-4-bets all
  legal); sampling makes the deep branches affordable.
- Uniform effective stacks, so all-in-for-less and reopening rules never
  arise.

Infosets are keyed by the packed public state (actor, folded/all-in masks,
commitments, bet, level) — histories differing only in when a never-invested
seat folded merge. Benign imperfect recall, standard for preflop solvers;
full-history keys are the memory-multiplying fallback if it ever bites.

Measured trees under the shipped menus (`preflop-gen tree`, pinned by
`shipped_tree_counts_are_pinned`):

| ruleset | decisions | distinct states | edges | fold-wins | all-in SD (multi) | flops (multi) | depth |
|---|---|---|---|---|---|---|---|
| cash100, pc-40, pc-60 | 155,492 | 99,023 | 332,966 | 21,988 | 135,834 (84,468) | 19,653 (12,762) | 21 |
| poker-chase-25 | 123,765 | 81,881 | 264,881 | 17,357 | 108,364 (67,636) | 15,396 (10,016) | 21 |

(40/60bb share cash100's shape — no menu size reaches the stack; at 25bb the
27.6bb 4-bet collapses into the jam.)

## Node addressing (path grammar)

Tokens `f | c | r<to-bb> | ai` joined by `-`; the root is the empty string.
Raise amounts are "raise to" in bb with trailing zeros trimmed (`r2.5`,
`r17.25`). The acting seat is implied by replaying the path (deterministic
order), and stored denormalized in each node. Example: `f-f-r2.5-f-c` =
folded to CO, CO opens 2.5bb, BTN folds, SB calls — BB to act.

## Output format (`src/preflop.rs`, format v1)

```
data/preflop/<ruleset>/header.json    # committed: config echo + hash + provenance + ev_unit
data/preflop/<ruleset>/starter.jsonl  # committed: nodes with reach >= starter_reach
data/preflop/<ruleset>/charts.jsonl   # gitignored: full export, reach >= export_reach
data/preflop/index.json               # committed: ruleset ids, written by `gen`
data/preflop/equity-hu-169.json       # committed: exact 169x169 HU equity table
```

One `PreflopNode` per JSONL line: `path`, `seat`, `pot_bb`, `to_call_bb`,
`reach` (max-over-classes arrival probability — the drill's sampling weight
and the export prune key), parallel `actions` labels, `freqs[action][169]`,
optional `evs[action][169]`. Class order is the 13×13 grid row-major: index
`= row*13 + col` over ranks A..2; diagonal pairs, upper triangle suited
(`class_name`/`class_index` in `src/preflop.rs` are the canonical mapping —
`web/app.js` iterates the same order). EVs are in `ev_unit`: `"bb"` for cash,
`"payout"` (units of the payout vector) for ICM. `PreflopNode::strategy_for`
adapts one class to `solution::NodeStrategy`, so the drills' EV-loss scoring
is reused unchanged.

Loaders read `starter.jsonl` and let a locally regenerated `charts.jsonl`
extend/override it. Unknown-field headers parse (forward compat); newer
`version` values are rejected.

Data policy: the starter tier **is committed** (the web browser on Pages and
a fresh-clone drill need it) — the inverse of `data/solutions/`. Never
hand-edit `data/preflop/**`; commit regens only when the manifest or solver
deliberately changed. Custom local rulesets are gitignored wholesale.

## Solver core (M2–M5)

- **Algorithm**: external-sampling MCCFR, regret-matching+, linear strategy
  averaging, seeded (deterministic per seed+budget). Per iteration a real
  52-card deck is dealt, giving exact card removal for free; per-action EV
  exports as average counterfactual value (`cfv_sum / weight`).
- **Terminals**: fold-wins pay the pot unraked. All-in showdowns use exact
  169×169 equity heads-up and a Monte-Carlo k-way cache (sorted class tuple
  → per-player pot share, disk-persisted) multiway. Non-all-in pots that see
  a flop are valued as equity × a static **realization factor** table (IP >
  OOP, playability-scaled) — *the* load-bearing approximation of the whole
  design; upgrade path: calibrate R against the in-repo `data/solutions/`
  postflop outputs (reading solve-gen JSON, no AGPL link). An `R ≡ 1.0`
  check-down baseline stays behind a flag for A/B.
- **ICM**: Malmuth–Harville over the paid places, applied at terminals.
  Split pots fold into the share vector; SeeFlop under ICM commits the
  ICM(E[stack]) ≈ E[ICM(stack)] approximation.
- **Convergence**: HU rulesets stop on exact best-response exploitability;
  6-max runs a fixed traversal budget and records checkpointed strategy
  drift in the header provenance.

## Consumers

- `drill preflop --ruleset <id>` (M6): sample a node by reach, deal a class
  combo, score EV-loss via `NodeStrategy` — retires the binary-chart drill.
  Stats records finally get preflop `ev_loss` and 169-class buckets.
- Web preflop tree browser (M7): pure-JS fetch of `starter.jsonl`, breadcrumb
  navigation, mixed-frequency 13×13 grids. Unstored deep lines render as
  "below starter reach".
- `data/ranges/` is **not** replaced: those files remain the postflop-solve
  inputs. A future `export-range` (weighted arrival ranges from a line) is
  named here, not built.

## Caveats (documented, deliberate)

- 3+-player CFR has no Nash-equilibrium guarantee; commercial preflop solvers
  ship the same caveat. Headers mark solves `cfr-approx-multiplayer`-style
  via provenance rather than claiming equilibrium.
- R-factor terminal valuation is an approximation (see above). Known leak:
  per-hero R without joint normalization is not exactly zero-sum.
- Tournament charts want per-seat stacks and future-game considerations
  eventually; the ladder of uniform-stack solves is the shipped
  approximation.

## Milestones

| # | lands | state |
|---|---|---|
| M1 | `src/preflop.rs` seam, `game.rs`, manifests, `tree`, pinned counts | ✅ |
| M2 | HU exact equity table + k-way MC cache; Malmuth–Harville + terminal valuer | — |
| M3 | MCCFR core; HU push/fold vs published Nash (`#[ignore]`) | — |
| M4 | R-factors, export, cash100 solve + committed starter, license test | — |
| M5 | Poker Chase ladder solves + ICM direction tests | — |
| M6 | `drill preflop` v2 (EV-loss, reach sampling) | — |
| M7 | web tree browser | — |
| M8 | docs sweep (00/02/06/README/skill) | — |
