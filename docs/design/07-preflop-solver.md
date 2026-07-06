# 07 — Preflop solver: solved 6-max charts (`crates/preflop-gen`)

Status: **shipped end to end** (M1–M8): solver, four committed rulesets,
EV-loss drill, web tree browser. The MCCFR core is validated against
published heads-up push/fold Nash; committed charts are shape-tested in CI.
Reverses the "preflop stays chart-data" stance of
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
`fivebet_mult`, `jam_from_level`), rake (`no flop no drop`), optional
`icm_payouts` (absent =
chip-EV cash), and `[solver]` knobs (traversals, seed, export/starter reach).
The header written next to every solve echoes the config verbatim and carries
its FNV-1a `config_hash`; `gen` skips rulesets whose hash already matches
(the solve-gen resumability contract).

Shipped rulesets: `cash100` (6-max 100bb, 5% rake capped at 3bb) and the
Poker Chase ladder `poker-chase-{10,25,40,60}` (0.25bb ante ⇒ 3bb root pot, ICM
payouts 4-2-1-0-0-0, no rake; 25bb offers the jam from vs-open on, 10bb is the
push/fold endplay rung with open-jams live from the start).

## The betting tree (`game.rs`)

A pure state machine — never materialized. Integer centi-bb pot math. Rules,
each a ruleset knob with a named ceiling:

- **No limps**: unopened action is fold-or-raise; a walk ends the hand before
  the BB ever acts unopened. (Upgrade: a limp token + check-closes-round.)
- Raise ladder: open (menu, absolute bb) → 3-bet (menu × the open; a single
  squeeze size once the open has a caller) → 4-bet (single size × the 3-bet)
  → 5-bet (single size × the 4-bet) → 6-bet jam-only. All-in joins the menu
  from `jam_from_level` on; menu sizes at/over the stack collapse into the
  jam. Facing a jam: fold/call.
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
| cash100 | 1,021,694 | 363,216 | 2,201,204 | 157,822 | 883,455 (532,350) | 138,234 (86,100) | 26 |
| poker-chase-60 | 810,252 | 305,959 | 1,744,958 | 124,460 | 701,118 (423,404) | 109,129 (68,146) | 26 |
| poker-chase-40 | 348,722 | 173,533 | 749,216 | 51,778 | 303,009 (185,338) | 45,708 (28,992) | 22 |
| poker-chase-25 | 162,411 | 99,793 | 348,131 | 23,315 | 141,799 (87,810) | 20,607 (13,262) | 22 |
| poker-chase-10 | 17,704 | 15,501 | 37,726 | 2,324 | 15,642 (10,048) | 2,057 (1,366) | 17 |

(The sized 5-bet level makes every depth distinct — deeper stacks keep more
5-bet/6-bet branches below the 4-bet. At 25bb only the largest line's 27.6bb
4-bet collapses into the jam; at 10bb the tree is push/fold — open, open-jam,
or fold.)

## Node addressing (path grammar)

Tokens `f | c | r<to-bb> | ai` joined by `-`; the root is the empty string.
Raise amounts are "raise to" in bb with trailing zeros trimmed (`r2.5`,
`r17.25`). The acting seat is implied by replaying the path (deterministic
order), and stored denormalized in each node. Example: `f-f-r2.5-f-c` =
folded to CO, CO opens 2.5bb, BTN folds, SB calls — BB to act.

## Output format (`src/preflop.rs`, format v1)

```
data/preflop/<ruleset>/header.json    # committed: config echo + hash + provenance + ev_unit
data/preflop/<ruleset>/starter.jsonl  # committed: nodes with reach >= starter_reach (0.002 ≈ 200-300 nodes, ~2 MB)
data/preflop/<ruleset>/charts.jsonl   # gitignored: full export, reach >= export_reach (0.0002)
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

- **Algorithm**: external-sampling MCCFR, regret-matching+, linearly
  weighted averaging with a **delayed-averaging warm-up** (first 20% of the
  budget updates regrets only, so averages and EVs never carry the early
  uniform-strategy noise). Seeded and single-threaded per solve
  (deterministic per seed+budget); parallelism is across rulesets — `gen`'s
  four manifests solve as independent processes. Per hand a real 52-card
  deck is dealt (exact card removal for free); per-action EV exports as
  average counterfactual value (`cfv_sum / weight`), a value vs the evolving
  average profile.
- **Terminals**: fold-wins pay the pot unraked. All-in showdowns use the
  exact 169×169 class table heads-up; 3+-way showdowns sample ~200 runouts
  **from the hand's actual remaining deck** per visit — unbiased,
  blocker-exact, bounded cost. (A memoized class-tuple cache was tried and
  lost: 5/6-way tuples almost never repeat, so it degenerated to one fresh
  20k-board estimate per terminal plus unbounded memory.) Non-all-in pots
  that see a flop are valued as equity × a static **realization factor**
  table (`r_factor`: playability × position × multiway; IP > OOP) — *the*
  load-bearing approximation of the whole design; upgrade path: calibrate R
  against the in-repo `data/solutions/` postflop outputs (reading solve-gen
  JSON, no AGPL link). An `R ≡ 1.0` check-down baseline stays behind
  `solve --check-down` for A/B.
- **ICM**: Malmuth–Harville over the paid places, applied at terminals.
  Split pots fold into the share vector; SeeFlop under ICM commits the
  ICM(E[stack]) ≈ E[ICM(stack)] approximation. `stack_bb` is the post-ante
  betting stack (antes are sunk, so they cancel from every action comparison
  while still inflating the contested pot).
- **Convergence**: HU rulesets have exact best-response exploitability
  (per-player, constant-sum-corrected). 6-max runs the manifest's hand
  budget (48M ≈ 15 min per ruleset in parallel) and records the probe-set
  strategy drift in the header provenance; macro shapes (RFI%, defense
  frequencies) stabilize long before the last mixed-frequency decimals —
  raise `traversals` when sharper mixes matter.

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
| M2 | HU exact equity table + k-way MC cache; Malmuth–Harville + terminal valuer | ✅ |
| M3 | MCCFR core; HU push/fold vs published Nash (`#[ignore]`) | ✅ |
| M4 | R-factors, export, `solve`/`gen` CLI, cash100 solve + committed starter, license test | ✅ |
| M5 | Poker Chase ladder solves + committed-chart shape tests | ✅ |
| M6 | `drill preflop` v2 (EV-loss, reach sampling, `--ruleset`) | ✅ |
| M7 | web tree browser | ✅ |
| M8 | docs sweep (00/02/06/README/skill) | ✅ |
