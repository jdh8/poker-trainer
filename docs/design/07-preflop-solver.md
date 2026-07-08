# 07 — Preflop solver: solved 6-max charts (`crates/preflop-gen`)

Status: **shipped end to end** (M1–M8): solver, a committed seven-rung cash
depth ladder, EV-loss drill, web tree browser. The MCCFR core is validated
against published heads-up push/fold Nash (the `no_limps` reference game);
committed charts are shape-tested in CI.
Reverses the "preflop stays chart-data" stance of
[02](02-solution-library.md)/[06](06-solver-capabilities.md) — for preflop
only. The postflop engine's limits are unchanged.

## Why a second generator crate

GTO-Wizard-style preflop charts (6-max, multiple open/3-bet sizes, ICM,
antes) cannot come from `postflop-solver`: it is heads-up, postflop-rooted
(`BoardState::Flop`), chip-EV only. No open-source alternative covers the
requirement, and vendor charts are ToS-restricted and can't express custom
rule sets (arbitrary depths, limps, ICM, antes) anyway. So `crates/preflop-gen` implements the
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
`fivebet_mult`, `jam_from_level`), `no_limps` (push/fold model — default off),
rake (`no flop no drop`), optional `icm_payouts` (absent =
chip-EV cash), and `[solver]` knobs (traversals, seed, export/starter reach).
The header written next to every solve echoes the config verbatim and carries
its FNV-1a `config_hash`; `gen` skips rulesets whose hash already matches
(the solve-gen resumability contract).

Shipped rulesets: the cash depth ladder `cash{5,10,15,20,32,50,75,100,150}` —
6-max, 5% rake capped at 3bb, chip-EV, limps enabled. 32bb is the geometric
mean of 50 and 20. `jam_from_level` rises with depth (150/100/75bb jam only vs
a 3-bet; 50/32bb offer the jam vs an open; 20bb and shorter are open-jam/fold),
so the short rungs are effectively push/fold while still allowing a limp.

## The betting tree (`game.rs`)

A pure state machine — never materialized. Integer centi-bb pot math. Rules,
each a ruleset knob with a named ceiling:

- **Limps**: an unopened seat may fold, limp (call the big blind), or open.
  In a limped, unraised pot the BB has its option — **check** (`x`, closes the
  round to a flop) or raise over the limpers (open menu, absolute bb). A walk
  still ends the hand when everyone folds to the BB. `no_limps = true` restores
  the classic push/fold tree (fold-or-raise, no BB option) — used by the HU
  Nash reference.
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
| cash150 | 7,281,536 | 1,184,149 | 15,762,495 | 1,199,455 | 6,242,838 (3,675,714) | 1,038,667 (626,276) | 31 |
| cash100 | 7,281,536 | 1,184,149 | 15,762,495 | 1,199,455 | 6,242,838 (3,675,714) | 1,038,667 (626,276) | 31 |
| cash75 | 7,281,536 | 1,184,149 | 15,762,495 | 1,199,455 | 6,242,838 (3,675,714) | 1,038,667 (626,276) | 31 |
| cash50 | 4,355,454 | 900,507 | 9,422,529 | 711,653 | 3,739,640 (2,210,154) | 615,783 (372,476) | 31 |
| cash32 | 2,320,842 | 629,260 | 5,014,533 | 372,881 | 1,997,466 (1,188,916) | 323,345 (197,074) | 27 |
| cash20 | 656,116 | 274,598 | 1,413,375 | 101,175 | 567,942 (343,725) | 88,143 (54,721) | 26 |
| cash15 | 364,328 | 188,438 | 784,131 | 55,507 | 316,002 (192,574) | 48,295 (30,072) | 22 |
| cash10 | 147,048 | 92,542 | 316,251 | 22,187 | 127,784 (78,421) | 19,233 (11,985) | 22 |
| cash5 | 24,380 | 20,446 | 51,987 | 3,259 | 21,494 (13,725) | 2,855 (1,881) | 17 |

(Limps + the BB option add the passive-pot branches at every depth, roughly
7× the old no-limp counts at 100bb. cash75/100/150 share an identical tree
shape — same `jam_from_level`, and no raise size reaches any of the stacks — so
only their equilibria differ. The `jam_from_level` ladder makes the shallower rungs
collapse the deep 4-bet/5-bet branches into the jam; at 5–20bb the tree is
essentially open-jam/fold plus the limp.)

## Limp scope — SB-only (decided 2026-07-08, not yet built)

The shipped ladder above enables limps at **every** seat, which *is* the ~7×
blow-up: early-position limps spawn multiway limped pots (4/5/6-way flops plus a
multiway BB option). Decided refinement: gate limps to **SB only** (the last
unopened non-BB seat). An SB complete is heads-up vs BB — one branch, a 2-way
flop — so this keeps the single strategically load-bearing limp while shedding
almost all of the 7×. Encoded as a limp-*scope* knob (`none` = today's
`no_limps` push/fold · `sb` = new default for the cash ladder · `all` = the
current multiway behaviour).

Deliberately **no per-seat open menus** — one global `open_to_bb = [2, 2.5, 3]`
for all seats, SB included. An isolated SB-vs-BB solve (100bb, cash rake, tested
under both the single- and three-size 3-bet menus) found SB's open sizing ≈
every other seat's: 2.5 and 3 both live, the 2bb open a harmless ~0.6%
EV-neutral crumb, and offering 3.5/4 drew ~27% frequency for **+0.002–0.004 bb**
(noise) — flat EV above 2.5bb, i.e. size-*indifference*, not a wish to open
bigger. SB's ideal menu `{limp, 2.5, 3}` is thus the global menu minus one 0.6%
button; a per-seat-menu refactor to delete that crumb isn't worth it. Building
`sb` scope will re-pin the tree counts and re-commit the ladder's starter
charts.

## Re-raise menu — 3-bet `{3, 4}`, multiplier-sized (decided 2026-07-08, not yet built)

Shipped `threebet_mult = [2, 3, 4]` carries a dead button and loses nothing by
trimming to two. From the SB-vs-BB and `[BTN,SB,BB]` diagnostics:

- **Drop the 2× min-3-bet.** ≤0.5% in *every* 3-bet spot measured — IP (BTN vs
  CO), both OOP blinds, and blind-vs-blind. A 2×-the-open 3-bet is too small to
  raise for value or fold equity, so the solver routes around it: dominated, not
  rare-but-crucial, so its near-zero frequency is a safe drop signal.
- **Don't add a 5×.** OOP piles onto the 4× ceiling (~8%), which *looks* like a
  wish to go bigger — but offering `{3,4,5,6}` makes the mass **spread evenly
  across 4/5/6** (not climb to 6×) for **+0.004 bb** (noise). Flat EV above 4×:
  size-indifference, not a binding ceiling. OOP wants *at least* 4× (3× is too
  small OOP) and nothing beyond it. Result: **`threebet_mult = [3.0, 4.0]`** —
  3× the IP size, 4× the OOP size, each earning its keep. 4-bets stay the single
  `[2.3]` (at 100bb the over-3-bet action is mostly fold/call/**jam** anyway).

Sizing stays **multiplicative** (× the open / × the prior raise), never absolute
bb: a reraise's correct size scales with the bet it raises over (a fixed-bb
3-bet is oversized vs small opens, undersized vs large ones). Opens stay
absolute-vs-BB because they have no prior raise to scale off — the deliberate
hybrid already in `game.rs`; pot-relative would be marginally more geometric but
isn't worth the machinery for the ante-free cash ladder.

**Methodology — the ceiling test.** Mass piling on a menu's top size is *not*
evidence the strategy wants bigger; as often it is flat-EV indifference. Raise
the ceiling and watch: mass that **climbs** to the new top is real; mass that
**spreads** is indifferent. This called three sizing decisions in a row (SB open
3.5/4, OOP 3-bet 5/6) — run it before widening any menu.

## Depth ladder — Fibonacci, shifted up one from HU (decided 2026-07-08, not yet built)

The shipped cash ladder `{5,10,15,20,32,50,75,100,150}` is ad-hoc — though the
doc already reaches for log-spacing ("32 is the geometric mean of 20 and 50").
Formalize that instinct. The HU reference is already pure Fibonacci
`heads-up{3,5,8,13,21,34,55,89}` (constant ×φ ≈ 1.618 per rung — uniform
log-spacing of stack depth). Give 6-max the **same ladder shifted up one
Fibonacci index**:

```
HU:     3   5   8  13  21  34  55  89
6-max:      5   8  13  21  34  55  89  144
```

Drop the 3bb jam-only floor (HU spin/hyper territory, not 6-max cash) and add a
144bb ceiling (≈ the retired 150 top). Same rung count, φ-spaced, and the two
formats stay one clean step apart, so "HU-89 vs 6max-89" names one depth.

- **89 replaces 100 as the flagship.** Fibonacci brackets 100 with 89 and 144;
  snap to 89 (~12% shallower, the nearest φ rung). No round-number depth is
  sacred to a trainer, and the SB-limp/`{3,4}` menu study (run at 100bb)
  transfers — 12% of depth doesn't move the sizing conclusions.
- **Keep the 5bb floor.** 6-way jam/fold ≠ HU jam/fold (Nash jam ranges tighten
  with more seats behind), and it's the cheapest tree in the ladder. Swapping the
  10/15/20/32/50/75 rungs for 8/13/21/34/55 is lateral, not a loss.
- **Cap at 144.** 233bb (next Fibonacci) is 200bb+ deep-cash niche and the most
  expensive tree (longest raise ladder before all-in) — add it when someone
  actually trains that deep.

**Zero marginal cost.** The SB-only-limp and `threebet_mult = [3, 4]` decisions
above already force a full re-solve of every rung; re-depthing to Fibonacci rides
that same pass, so the new depths cost no extra generation time. `jam_from_level`
re-assigns per new depth (deep rungs jam only vs a 3-bet, mid rungs offer the jam
vs an open, short rungs open-jam/fold — exact thresholds tuned at build). Re-pins
the shipped tree counts and re-commits the ladder's starter charts, same as the
menu changes.

## Node addressing (path grammar)

Tokens `f | c | x | r<to-bb> | ai` joined by `-`; the root is the empty string.
`c` is a call (a limp at the unopened root), `x` the BB's check. Raise amounts
are "raise to" in bb with trailing zeros trimmed (`r2.5`, `r17.25`). The acting
seat is implied by replaying the path (deterministic order), and stored
denormalized in each node. Example: `f-f-r2.5-f-c` = folded to CO, CO opens
2.5bb, BTN folds, SB calls — BB to act; `c-c-c-c-c-x` = limped around, BB
checks its option to a six-way flop.

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
  (deterministic per seed+budget); parallelism is across rulesets — the
  ladder's manifests solve as independent single-threaded processes (one core
  each; fan out under `idle-run.sh` on a shared box). Per hand a real 52-card
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
  (per-player, constant-sum-corrected). 6-max runs the manifest's hand budget
  (500M hands; the deep limp-inclusive rungs are ~1.18M states, so a fanned-out
  batch of all nine is ~10 h wall-clock on spare cores) and records the
  probe-set strategy drift in the header provenance. Caveat: that drift is an
  L∞ (max) over a handful of high-reach probe nodes, so it floors around
  0.06–0.10 — one genuinely-indifferent hand pins it — and does **not** track
  distance to equilibrium. It barely moved 48M→500M, yet the averaged charts
  shifted materially (~10–12% of action-probabilities by >0.15; some hands
  flipped), because the later, wider Cesàro window is closer to equilibrium.
  Macro shapes (RFI%, defense frequencies) stabilize by ~48M; the fine mixes
  need the full budget. Raise `traversals` further only if the rarely-reached
  limp lines still look noisy — there is no cheap header signal for it, diff
  the charts against a hotter solve to tell.

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
| M5 | cash depth ladder solves (limps + BB option) + committed-chart shape tests | ✅ |
| M6 | `drill preflop` v2 (EV-loss, reach sampling, `--ruleset`) | ✅ |
| M7 | web tree browser | ✅ |
| M8 | docs sweep (00/02/06/README/skill) | ✅ |
