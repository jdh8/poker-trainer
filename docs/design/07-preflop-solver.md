# 07 — Preflop solver: solved 6-max charts (`crates/preflop-gen`)

Status: **shipped end to end** (M1–M8): solver, a committed eight-rung cash
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

Shipped rulesets: the cash depth ladder `cash{5,8,13,21,34,55,89,144}` — 6-max,
5% rake capped at 3bb, chip-EV, SB-only limps, 3-bet menu `{3, 4}×`. A Fibonacci
ladder (each rung ≈ ×φ), shifted one index up from the HU set so 89bb is the
flagship and 144bb the deep rung (decisions below). `jam_from_level` rises with
depth (144/89/55bb jam only vs a 3-bet; 34bb offers the jam vs an open; 21bb and
shorter are open-jam/fold), so the short rungs are effectively push/fold while
still allowing the SB's limp.

The tournament ladder `mtt{5,8,13,21,34,55,89,144}` mirrors it — same 6-max seats,
depths, menus and SB-only limps — but swaps cash's rake for a **1 BB Big Blind
Ante** (`ante_bb = 1.0`, chip-EV, no ICM). `ante_bb` is the *total* dead ante
(not per-player), so a BBA is one field independent of seat count; because the
total is table-size-independent, each 6-max chart already subsumes the
short-handed endgame (its UTG+HJ-fold node *is* the 4-max BBA game). See §Rake &
ante.

## Rake & ante — real-world calibration

Both economy knobs are pinned to a 2024–2026 survey of popular platforms and series
(full cited version: [../rake-ante-survey-2026.md](../rake-ante-survey-2026.md)):

- **Rake — validated, no change.** Online cash is near-universally ~5% "no flop no drop"
  with a small cap; the ladder's `rake_rate = 0.05` / `rake_cap_bb = 3.0` / unraked
  fold-wins is literally GGPoker Rush & Cash's structure (5% capped at 3bb). PokerStars
  ≈ $2.50 cap at NL100, WPT Global 4%, ACR flat $3 — all the same shape. Rake **tightens**
  ranges (marginal opens/flats that break even at 0 rake go negative), the mirror of the
  ante below.
- **Ante — `0` is right for cash, universal for tournaments.** Standard NLHE cash is
  blinds-only (antes are a niche high-stakes-live novelty). But every serious tournament
  (WSOP/EPT/WPT/Triton NLH since ~2018–19, plus PKO/satellite/turbo) runs an ante — live
  via the **big blind ante = 1bb**, online via a per-player ~⅛-bb ante; both put ≈1bb of
  dead money in the pot, making it ~40% bigger preflop and **widening** correct ranges.

**Tournament ladder — shipped** (`mtt{5..144}`, chip-EV). Its regime barely overlaps cash —
a tournament's decision-weighted play sits at **~10–40bb with an ante**, not 100bb without
one — so it is a distinct chart set, not a variation. It sets `rake_rate = 0` (tournament
juice is an entry fee, not a per-pot drop) and a **1 BB Big Blind Ante**. `ante_bb` is now
the *total* dead ante (`ante_bb = 1.0`), not per-player: a BBA is one table-size-independent
field, and for chip-EV who posts is irrelevant (the "only-BB-posts" mechanic matters only
under ICM, which the chip-EV tier defers). Because the total ante is table-size-independent,
each 6-max chart **subsumes the shorter tables** — its UTG+HJ-fold node (pot
`0.5 + 1.0 + 1.0 = 2.5bb`, CO/BTN/SB/BB to act) *is* the 4-max BBA game — so no separate
short-handed tier is needed. Cash/HU tiers are unaffected (`ante_bb = 0` either way). A true
ICM tier (add `icm_payouts`, where BB-posting would then matter) stays the open extension.

## The betting tree (`game.rs`)

A pure state machine — never materialized. Integer centi-bb pot math. Rules,
each a ruleset knob with a named ceiling:

- **Limps** (`limp_scope`): an unopened seat may fold, limp (call the big
  blind), or open. `all` lets every seat complete (the multiway limped pots);
  `sb` (the cash-ladder default) keeps only the small blind's completion — one
  2-way branch; `none` is the classic push/fold tree (fold-or-raise, no BB
  option), used by the HU Nash reference (`no_limps = true` is the back-compat
  shorthand for it). In a limped, unraised pot the BB has its option —
  **check** (`x`, closes the round to a flop) or raise over the limpers (open
  menu, absolute bb). A walk still ends the hand when everyone folds to the BB.
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
| cash144 | 885,384 | 311,682 | 1,907,869 | 137,107 | 765,294 (460,644) | 120,085 (74,724) | 26 |
| cash89 | 885,384 | 311,682 | 1,907,869 | 137,107 | 765,294 (460,644) | 120,085 (74,724) | 26 |
| cash55 | 673,940 | 254,423 | 1,451,617 | 103,743 | 582,956 (351,698) | 90,979 (56,770) | 26 |
| cash34 | 175,394 | 105,715 | 376,051 | 25,269 | 153,046 (94,580) | 22,343 (14,370) | 22 |
| cash21 | 70,076 | 50,301 | 149,853 | 9,707 | 61,456 (38,540) | 8,615 (5,632) | 21 |
| cash13 | 18,832 | 16,936 | 40,017 | 2,359 | 16,714 (10,848) | 2,113 (1,440) | 16 |
| cash8 | 3,572 | 3,572 | 7,569 | 431 | 3,192 (2,128) | 375 (254) | 12 |
| cash5 | 1,876 | 1,876 | 3,945 | 199 | 1,696 (1,164) | 175 (126) | 11 |

(SB-only limps keep just the one 2-way completed branch — the multiway limped
pots that made the old all-seat tree ~8× larger at ~100bb are gone. cash89/144
share an identical tree shape — same `jam_from_level`, and no raise size reaches
either stack — so only their equilibria differ. The `jam_from_level` ladder makes
the shallower rungs collapse the deep 4-bet/5-bet branches into the jam; at
5–21bb the tree is essentially open-jam/fold plus the SB limp.)

## Limp scope — SB-only (shipped 2026-07-08)

Limps are gated to the **SB only** (`limp_scope = "sb"`, the cash-ladder
default). All-seat limps *were* the ~7× blow-up: early-position limps spawn
multiway limped pots (4/5/6-way flops plus a multiway BB option). An SB complete
is heads-up vs BB — one branch, a 2-way flop — so `sb` keeps the single
strategically load-bearing limp while shedding almost all of it (the measured
shrink at ~89–100bb was ~8×). The knob's other settings: `none` = push/fold (the
HU Nash reference; `no_limps = true` is the shorthand) · `all` = the full
multiway behaviour.

Deliberately **no per-seat open menus** — one global `open_to_bb = [2, 2.5, 3]`
for all seats, SB included. An isolated SB-vs-BB solve (100bb, cash rake, tested
under both the single- and three-size 3-bet menus) found SB's open sizing ≈
every other seat's: 2.5 and 3 both live, the 2bb open a harmless ~0.6%
EV-neutral crumb, and offering 3.5/4 drew ~27% frequency for **+0.002–0.004 bb**
(noise) — flat EV above 2.5bb, i.e. size-*indifference*, not a wish to open
bigger. SB's ideal menu `{limp, 2.5, 3}` is thus the global menu minus one 0.6%
button; a per-seat-menu refactor to delete that crumb isn't worth it, so `sb`
keeps the one global menu and just drops the non-SB limps.

## Re-raise menu — 3-bet `{3, 4}`, multiplier-sized (shipped 2026-07-08)

The ladder ships `threebet_mult = [3, 4]`; the old `[2, 3, 4]` carried a dead
button and lost nothing by trimming to two. From the SB-vs-BB and `[BTN,SB,BB]`
diagnostics:

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

## Depth ladder — Fibonacci, shifted up one from HU (shipped 2026-07-08)

The old cash ladder `{5,10,15,20,32,50,75,100,150}` was ad-hoc — though it
already reached for log-spacing ("32 is the geometric mean of 20 and 50"). The
ladder now formalizes that instinct. The HU reference is already pure Fibonacci
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

**Zero marginal cost.** The SB-only-limp and `threebet_mult = [3, 4]` changes
already forced a full re-solve of every rung; re-depthing to Fibonacci rode that
same pass, so the new depths cost no extra generation time. `jam_from_level` is
assigned per depth: 144/89/55 jam only vs a 3-bet, 34 offers the jam vs an open,
21 and shorter are open-jam/fold. Solved at 1G traversals/rung — a 10× bump on
the initial 100M pass; the ~8× smaller SB-only tree already converged as well at
100M as the old all-limp ladder did at 500M, so 1G is headroom for the rare
multiway lines.

## Node addressing (path grammar)

Tokens `f | c | x | r<to-bb> | ai` joined by `-`; the root is the empty string.
`c` is a call (a limp at the unopened root), `x` the BB's check. Raise amounts
are "raise to" in bb with trailing zeros trimmed (`r2.5`, `r17.25`). The acting
seat is implied by replaying the path (deterministic order), and stored
denormalized in each node. Example: `f-f-r2.5-f-c` = folded to CO, CO opens
2.5bb, BTN folds, SB calls — BB to act; `f-f-f-f-c-x` = folded to the SB, SB
completes, BB checks its option to a heads-up flop (the only limp `sb` scope
allows; `all` scope also reaches multiway limps like `c-c-c-c-c-x`).

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
  (1G hands) and records the probe-set strategy drift in the header
  provenance. Wall-clock is **terminal-bound, not tree-bound**: the multiway
  all-in Monte-Carlo dominates, so push/fold rungs — which funnel most
  traversals into 3–6-way jams — cost the most despite the smallest trees. At
  1G, single-threaded, the eight rungs run ~8.7–13 h each (cash5, 1.9k states,
  is the *slowest* at ~13 h; cash144, 885k states, is not), fanned across spare
  cores as one overnight batch. Caveat: the drift is an L∞ (max) over a handful
  of high-reach probe nodes, so it floors around 0.06–0.09 — one
  genuinely-indifferent hand pins it — and does **not** track distance to
  equilibrium; macro shapes (RFI%, defense frequencies) stabilize early while
  the fine mixes keep moving. Push beyond 1G on just the deep rungs (55/89/144)
  if the rarely-reached lines still look noisy — there is no cheap header
  signal, diff against a hotter solve to tell.

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
