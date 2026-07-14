# 08 — Instant flop tables (canonical store + suit relabeling)

Status: **shipped** (lookup, generation machinery, grounded lines, local web);
the table store itself fills over weeks of idle generation.

## The problem

Commercial tools (GTO Wizard) feel instant because they never solve at lookup
time. Here, picking a preflop line and entering a flop meant a 30 s–4 min
live solve at ~9 GB RSS. The reach-pruned tables (doc 00, P-tables) already
made *generated* flops instant — but only for the exact flop strings a
manifest had listed, in the exact card order the manifest spelled them.

## The idea

Two facts compose into full coverage:

1. **1,755 classes.** The 22,100 real flops collapse to 1,755 under suit
   relabeling (`iso_flops()` in solve-gen enumerates them; canonical form =
   smallest sorted card-id triple over the 24 suit permutations).
2. **Every range in play is class-level** (`"AA:0.62,AKs:…"` — formation
   range files and `weighted_range_string` from preflop charts alike), hence
   suit-symmetric. So a canonical-flop equilibrium transfers **exactly** to
   any real flop by relabeling suits. Not an approximation.

Solve the 1,755 canonical flops per config offline; at lookup, map the
entered flop onto its class representative and relabel the stored answer
back. Every flop is then a table read.

## Lookup (`src/iso.rs`, trainer side — pure card math, no solver link)

- `canonical_flop(flop) → (canonical, SuitPerm)`, byte-matching solve-gen's
  enumeration (pinned by `trainer_iso_agrees_with_iso_flops` in solve-gen,
  which sees both sides). Ties — paired boards, the unused 4th suit — resolve
  to the first minimizing perm in enumeration order; any minimizer is
  game-exact, determinism is just reproducibility.
- `find_table` (src/trainer.rs) resolves a flop in three steps: exact stem →
  canonical stem (how all-1755 files are named) → directory scan (legacy
  tiers keep the manifest's card order/suits in their stems; any stem in the
  same class serves through the composed user→stored perm).
- `TableWalk` (src/tree.rs) carries the perm. Outbound nodes translate
  board / hands / dealable / `"deal X"` line labels; inbound `deal(card)`
  maps user→stored. **Two orderings are load-bearing, not cosmetic**
  (`translate_node`, src/iso.rs):
  - hands re-render high-card-first and re-sort ascending by `(low, high)`
    card id, with `freqs`/`evs` columns and `weights`/`equity` permuted in
    lockstep — the lock editor ships `[action][hand]` strategies
    index-parallel to a live game's hand order, and the hand drill looks
    hands up by string equality across a live fallback;
  - board (flop cards) and `dealable` sort ascending by card id — matching
    what a live solve of the raw flop renders, keeping stats and lock files
    identical across table/live paths.
- The live path stays untranslated: `go_live` solves the **user's raw flop**
  (relabeling stored deal labels during line replay), so lock / resolve /
  runouts / `TreeSession` needed zero changes.
- Passive lenses must not pay for the fallback: `TreeWalk::peek` answers
  child lookups from the table only, and the blockers lens quietly hides off
  the stored frontier instead of triggering a solve on every navigation.

The end-to-end exactness proof is the `#[ignore]` test
`iso_table_walk_matches_live_solve_of_raw_flop`: a table generated for
Td9d6h, opened as Ts9s6h, equals a fresh live solve of Ts9s6h
element-for-element.

## Generation at scale

- `manifests/all-1755.toml`: `flops = "all-iso-flops"` per formation.
  Measured ≈ 200–300 s per flop under SCHED_IDLE → **4–6 idle days and
  ~20–80 GB of JSONL per config**. Resumable per flop (the `.jsonl` is the
  gate); kill/reorder freely.
- `tables --no-save-bins` is **mandatory for bulk runs**: a full-precision
  solver save is 0.65–12 GB per flop — the full tier would write 10+ TB of
  cache. Existing bins still load (warm hits for the old texture-25 spots).
- Storage lives on the bulk HDD: `data/tables → /srv/var/poker/tables` and
  `~/.cache/poker-trainer/solves → /srv/var/poker/solves` (symlinks; the
  btrfs `zstd:1` mount compresses the JSONL further). `.gitignore` uses
  `/data/tables` without a trailing slash so the symlink stays ignored.
- Always idle-wrapped, one job at a time
  ([shared-machine-data-gen](../shared-machine-data-gen.md)):

  ```sh
  scripts/idle-run.sh cargo run -p solve-gen --release -- tables \
    --manifest manifests/all-1755.toml --no-save-bins
  ```

## Grounded preflop lines (`src/ground.rs`)

The "pick a preflop line" half. `ground("<ruleset>:<line>", preflop_root)` is
the **single constructor** turning a flop-closing line into a `SpotConfig`:
formation = the spec itself, solver-default sizes, ranges/pot/stack/rake from
the chart equilibrium. The trainer's `--from`, solve-gen's `--from`
(solve + tables), and manifest `from =` runs all call it — the config hash
(which keys data/solutions, data/tables, and the solve cache) aligns across
every path by shared code, not convention. `export-range` emits the
hash-aligned `solve --flop X --from <spec>` command and lists a ruleset's
lines ranked by `PreflopCharts::line_mass` (product of every acting seat's
combo-weighted arrival marginal — class-level, ranking-grade).

On disk a grounded config's directory is `formation_dir(spec)` =
`cash-hu55_r2.5-c` (`:` is Windows-illegal; `_` collides with nothing; a
no-op for curated ids, so existing trees stayed valid). Line tiers:
`manifests/lines-cash-hu55.toml` (every line ≥ 0.1% mass),
`lines-cash89.toml` / `lines-mtt89.toml` (top 10 by mass), each
× all-iso-flops, queued behind the formations.

`curated_formation_hashes_are_pinned` (src/solution.rs) pins the five curated
hashes so nothing silently re-keys hundreds of GB of artifacts. (The old
`--from` shell configs — srp-btn-bb formation id — were re-keyed by this
design; those cache entries were orphaned deliberately.)

## Local web

Pages keeps the committed texture-25 `data/tables-web` tier; the full store
is far too big to deploy, so breadth is local-only. `scripts/serve-web.sh`
stages `/srv/var/poker/tables-web` (`export-tables-web --out`, rerun with
`--export` as generation lands) and serves on :8000. The tables browser's
"Any flop" input canonicalizes via the wasm `canonical_flop` export, finds a
stored stem in the same class, composes the stored→user suit map in JS, and
relabels board/hand strings before rendering (grids rebucket by hand class,
so no index-parallel hazard). An inline hint names the serving stem and marks
the result exact.

## Known ceilings (deliberate)

- **River and off-frontier lines live-solve the raw flop, cold** — bulk runs
  save no bins, so the first off-path op on a fresh flop is a full solve.
  Upgrade path: warm-start `resolve` (the existing `ponytail:` in solve-gen)
  or river sub-solves from stored turn state. Not scheduled.
- `serve-web.sh --export` re-walks every table jsonl (no resume gate); add a
  per-file mtime gate if it drags.
- `drill gto` / `drill range` snapshots (`data/solutions`) don't canonicalize
  yet — same trick applies to `LiveSolutionProvider` lookups (~½ day, no
  index hazard: `SolvedSpot` grids rebuild from hand strings).
- Line mass is a product of per-seat marginals — class-level card removal
  and cross-seat correlation ignored, like the charts themselves.

## Cost ledger

| tier | solves | idle time | disk (logical) |
|---|---|---|---|
| one formation × all-1755 | 1,755 | ~4–6 days | ~20–80 GB |
| five formations | 8,775 | ~3–4 weeks | ~220 GB |
| one grounded line | 1,755 | ~4–6 days | ~20–70 GB |
| shipped line tiers (28 lines) | ~49k | ~4–6 months | ~1–1.5 TB |

All resumable, mass-ordered, and interruptible; `/srv` (2.4 TB free at
adoption, zstd-compressed) absorbs the full ledger. Trim the line manifests
rather than letting the queue run past what you actually drill.
