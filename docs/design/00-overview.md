# Design 00 — Commercial-parity overview

Goal: reach practical feature parity with commercial poker study tools — GTO
Wizard is the reference, with nods to PioSOLVER where it's the better model —
as a **local, CLI/TUI, scriptable** tool. Parity means matching the four
product pillars (Study, Practice, Analyze, custom solving), not the SaaS
around them.

This doc is the map: the parity matrix, the invariants every phase must keep,
the phase sequence, and the honest out-of-scope list. Each area has its own
design doc (`01`–`06`).

## Invariants (every phase must keep these)

1. **License seam.** The trainer never links `postflop-solver`. The solver is
   only ever reached across a process boundary (today: shell-out; next: a
   long-lived subprocess speaking a JSON protocol — see 01). AGPL stays inside
   `crates/solve-gen`.
2. **Neutral, versioned data formats.** Everything the trainer reads is a
   format defined in `src/solution.rs` (or a sibling), never the solver's own
   serialization. Formats carry a version and old files keep parsing.
3. **CLI-first.** Every feature is scriptable (flags, stdin/stdout, CSV/JSONL
   out). The TUI is a view over the same data, never the only door. This is
   where we *beat* GTO Wizard: Pio-style scripting falls out for free.
4. **Local and offline.** No accounts, no network. Long solves are a fact of
   life on local hardware; the design answer is caching + curated snapshots,
   not a server farm.

## Parity matrix

| GTO Wizard capability | Today | Plan |
|---|---|---|
| Browse strategy grid (13×13) per node | ✅ `table`, 3 nodes/flop | — |
| Browse the **whole game tree** (any line, any runout) | ✅ `table --board` tree browser | — |
| Range / EV / equity views, filters | ✅ `s/w/e/y` lenses + `f` bucket filter | — |
| Runouts report (strategy across all turn/river cards) | ✅ `o` runouts view at chance nodes | — |
| Aggregate flop reports (all flops, sortable by texture) | ✅ `report` (+`--csv`, texture rollups) | — |
| Blockers / range-vs-range equity tools | ✅ range-vs-range `equity` (+histogram); blockers pending | P8 → [03](03-study-mode.md) |
| Single-node drills, EV-loss scoring | ✅ `drill gto` | — |
| Range-builder drills + leak buckets | ✅ `drill range` | — |
| **Full-hand practice** (flop→river vs. equilibrium villain) | ✅ `drill hand` (`--board` spots; library sampling needs P6) | — |
| Persistent session stats, leak trends | ✅ `stats` over `history.jsonl` | — |
| Preflop charts + preflop drills | partial (charts in `data/ranges/`, 5 formations) | `drill preflop` → [04](04-training-mode.md) |
| Formation breadth (positions, 3-bet pots, stack depths, rake) | ✅ config-side (5 formations, rake, manifests); breadth tiers solve locally | data-gen → [02](02-solution-library.md) |
| Hand-history import & leak analysis | partial (`analyze --dry-run` import + coverage) | P9 scoring → [05](05-analyze.md) |
| Custom spot solving (ranges/sizes/stacks) | ✅ `--board` + knobs | — |
| **Nodelocking** (lock villain, re-solve exploit) | ❌ (solver supports it) | P10 → [06](06-solver-capabilities.md) |
| ICM / MTT postflop | ❌ (engine lacks it) | research → [06](06-solver-capabilities.md) |
| Multiway postflop | ❌ (engine is 2-player) | out of scope → [06](06-solver-capabilities.md) |

## The keystone: from extracted nodes to tree sessions

Today one solve is flattened into three `SolvedSpot` JSON snapshots and the
solved tree is thrown away. Almost every missing feature above — tree
browsing, runouts, multi-street drills, hand-history scoring, nodelocking —
needs access to *arbitrary nodes* of the solved tree.

Exporting full trees is the obvious-but-wrong fix: a Pio-style full save runs
50–500 MB **per flop**. The design answer (doc 01) is to keep the solved game
hot inside a long-lived `solve-gen serve` subprocess and query nodes over
stdio JSON. The license seam is untouched (process boundary), no new tree
format exists, and node queries are instant once the ~30 s–4 min solve (or a
cache hit) completes. `SolvedSpot` snapshots remain the instant, offline path
for curated drills.

## Phases

Continues the README's phases 0–3. Sizes are relative (S/M/L).

| Phase | Deliverable | Size | Needs | Doc |
|---|---|---|---|---|
| **P4** | `solve-gen serve` + `TreeSession`: query any node of a solved game | L | — | [01](01-tree-protocol.md) |
| **P5** | Multi-street full-hand drill + persistent stats/leak profile | M | P4 | [04](04-training-mode.md) |
| **P6** | Library v2: formations, preflop chart files, manifests, rake, config-hash cache keys | M | — (parallel to P4) | [02](02-solution-library.md) |
| **P7** | Study browser v2: tree walking, range/EV/EQ views, runouts | M | P4 | [03](03-study-mode.md) |
| **P8** | Aggregate flop reports + equity/blocker tools (`report`, `equity` done; blocker column pending) | M | P6 | [03](03-study-mode.md) |
| **P9** | `analyze`: hand-history import, EV-loss + leak report | L | P4, P6 | [05](05-analyze.md) |
| **P10** | Nodelocking end-to-end (lock, re-solve, compare) | M | P4, P7 | [06](06-solver-capabilities.md) |
| — | ICM (solver fork), bunching, multiway | research | — | [06](06-solver-capabilities.md) |

P4 and P6 are independent and both unblock most of the rest; do P4 first —
it's the keystone.

## Risks

- **Solve latency locally.** 30 s–4 min per uncached spot. Mitigations:
  config-hash cache of solver saves (01), curated snapshot library (02), and
  an explicit `--solve-budget` in analyze (05). Never pretend to be fast.
- **Upstream solver pin.** `postflop-solver` ships breaking changes without
  version bumps; we pin a rev. Vendoring is the escape hatch if the repo
  disappears. Capability facts in doc 06 are verified against the pinned rev.
- **Data growth.** ~200 KB JSON per node snapshot today; a 95-flop ×
  2-formation library ≈ 100–150 MB. Fine. Trigger for compression/indexing:
  library > 1 GB or load > 2 s (02).
- **HH format drift.** Site exporters change; parser is versioned golden-file
  tested (05).

## Out of scope (deliberately, with reasons)

- **Web/GUI, cloud, accounts, content library** — different product. CLI/TUI
  covers the study loop; a GUI can wrap the CLI later without new core.
- **Multiway postflop solving** — the engine is 2-player and multiway
  equilibria lack Nash guarantees anyway; even commercial multiway is
  approximate. Revisit only if a credible engine appears (06).
- **Solve-speed parity with GTO Wizard AI** — theirs is a datacenter + NN
  approximator. Local caching is our answer, not model inference.
- **Real-time play assistance (HUD)** — out; ethically and ToS-fraught.
