# Design 05 — Analyze: hand-history import & leak report (P9)

**Depends on:** 01 (scoring off-tree decisions), 02 (formation matching),
04 (shared stats aggregator). The last big pillar; also the most
approximation-laden — this doc is explicit about what the numbers mean.

## Problem

Drills measure you on invented spots. The leak report that matters comes from
the hands you actually played. GTO Wizard Analyze: upload histories, get
per-decision EV loss and aggregated leaks. Ours is
`poker-trainer analyze <files…>`, local.

## Pipeline

parse → normalize → match → score → report. Each stage is a pure function
with its own tests; only *score* touches a solver.

### Parse

PokerStars text format first (the de-facto interchange format; several sites
export it). Extract only: stakes, seats/positions, stacks, hero cards,
preflop actions, board, postflop actions, showdown. A few hundred lines of
hand-rolled parsing; no maintained Rust HH-parser crate is worth adopting
(evaluate again at build time; adopt one only if it's genuinely maintained).
Golden-file tests on small **synthetic** fixtures — never commit real HHs.
GGPoker format second.

### Normalize & match

- Keep hands that are **heads-up by the flop** in a library formation
  (SRP/3BP between covered seats). Everything else is counted and skipped —
  the coverage line in the report keeps us honest.
- Stacks bucket to the nearest library depth (e.g. 60–150bb → 100bb config);
  pot mismatches beyond ±25% → skip. Bet sizes map to the tree's nearest size
  (standard solver-analysis practice; noted in the report footer).
- Result per hand: a `SpotConfig` + the action line + hero's decisions.

### Score

For each decision, read the node off a `TreeSession` for that config and
board. This is where cost lives: uncached (formation, flop) pairs cost a
solve. Quantizing configs (stack buckets, canonical sizes) makes cache hits
the common case after warm-up.

- `--solve-budget <duration>` (default ~10 min): solve cache misses
  most-frequent-flop-first until the budget is spent; remaining decisions are
  reported as *unscored*, never guessed.
- Coverage always printed: `scored 61% of decisions (n=1,204); 22% unscored
  (budget); 17% skipped (multiway/uncovered formation)`.

### Report

The 04 aggregator over analyze records (`source:"analyze"`, kept out of the
drill history by default): EV loss by street/texture/bucket/formation,
blunder list sorted by bb lost with the full line printed for replay in
`table`. `--jsonl out` dumps records for external tooling.

## What the numbers mean (printed in the report footer)

EV loss is measured **against the library's equilibrium ranges**, not your
opponents' actual ranges. That's a study signal (where you diverge from
baseline), not a winrate audit. Range drift compounds street by street:
opponents' real actions re-weight ranges differently than equilibrium does.
Node-locked re-analysis (06) is the eventual answer for known-bad pools;
until then the honest framing is "distance from GTO", which is exactly what
commercial analyzers report too.

## Milestones

1. PS parser + normalizer with golden tests; `analyze --dry-run` prints
   match/coverage stats only (no solver) — useful on day one to size the
   library gap (02 feedback loop).
2. Scoring via sessions + budget; report + blunder list.
3. GGPoker parser.
4. `--jsonl` export; `table` replay handoff (`--line`).

## Out of scope

- Multiway pots, limped pots, ICM tournaments — match what the library
  covers; widen via 02/06, not analyzer special cases.
- Auto-import watchers, database sync, tracker integration (HM/PT4 —
  their DBs are undocumented and shifting; text HH files are the stable API).
- Opponent profiling / pool stats (that's a tracker; we measure *hero*).
