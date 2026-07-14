# Design 02 — Solution library v2: formations, ranges, manifests (P6)

**Status: shipped** through milestone 4 (`texture-25` solved locally); the
`broad-95` / `all-1755` tiers have no manifests yet. This doc is design
rationale; current state lives in [00-overview](00-overview.md).

**Unlocks:** aggregate reports (03), preflop drills (04), analyze matching
(05). **Depends on:** nothing (parallel to P4).

## Problem

The library is one formation (BTN-vs-BB SRP), one stack depth, rake-free,
8 flops, with two ranges hardcoded as `const` strings and all metadata encoded
in filenames (`td9d6h-oop-33.json`). Parity needs breadth — and machine-usable
provenance on every file.

## `SpotConfig`: one struct, everywhere

The config that today exists implicitly across CLI flags, `Spot`, and
filenames becomes one serde struct in `src/solution.rs` (trainer-owned, so
both crates share it via the existing dependency direction):

```rust
pub struct SpotConfig {
    pub formation: String,      // "srp-btn-bb", "3bp-bb-btn", ...
    pub oop_range: String,      // range string (possibly resolved from a named file)
    pub ip_range: String,
    pub flop_sizes: String,     // "33%, 75%"
    pub turn_sizes: String,     // now configurable, default "33%"
    pub river_sizes: String,
    pub stack_bb: f32,
    pub pot_bb: f32,
    pub rake_rate: f32,         // TreeConfig already supports both; plumb through
    pub rake_cap_bb: f32,
}
```

It is: the `serve`/`solve` request body (01), the cache key input (canonical
JSON → hash), and embedded in every snapshot: `SolvedSpot` gains
`config: Option<SpotConfig>` + `generator: Option<GenInfo>` (solve-gen
version, exploitability reached). `Option` keeps old files parsing.

**Cache-key fix:** custom solves currently write `<flop>-ip.json`, silently
overwriting the curated snapshot for that flop. New naming:
`<flop>-<confighash8>-<node>.json`, with the default-config hash spelled out
in the manifest so curated names stay predictable. Lookup goes through
embedded `config`, not filename parsing.

## Preflop ranges as data

`data/ranges/<formation>/<seat>.txt` — one range string per file, the format
the solver already parses. The two `const`s become
`data/ranges/srp-btn-bb/{oop,ip}.txt`. Formations reference ranges by path;
`--oop`/`--ip` still override inline.

Curate our own charts (start: the current two, plus CO-vs-BB, SB-vs-BB,
BB-vs-BTN-3bet). Do **not** copy commercial solver outputs wholesale —
individual frequencies are facts, but bulk-copying a charted product is
legally gray and against the project's clean-license posture. Hand-curated
training-grade charts are fine; solving preflop is out of scope *for the
postflop solver* (06) — it now has its own permissive MCCFR crate,
`preflop-gen` ([07](07-preflop-solver.md)), whose solved charts live under
`data/preflop/` and are **not** derived from, nor a replacement for, these
range files.

These files remain the postflop solves' input ranges. `drill preflop` now
scores against the solved `data/preflop/` charts instead (07); a future
`export-range` condensing a solved line into weighted arrival ranges is named
in 07, not built.

## Formations

A formation = preflop line + seats + default pot/stacks. v2 targets, in order
of real-hand frequency:

| id | pot type | default pot/stacks |
|---|---|---|
| `srp-btn-bb` | single-raised | 6bb / 97bb (today's) |
| `srp-co-bb`, `srp-sb-bb` | single-raised | 6bb–5.5bb / ~97bb |
| `3bp-bb-btn` | 3-bet | ~18bb / 89bb |
| `3bp-btn-co` | 3-bet | ~20bb / 89bb |

Plus stack-depth variants (40bb, 200bb) as manifest entries, not new code.
Rake presets (e.g. NL50: 5% capped 3bb) attach per manifest.

## Manifests

`manifests/<name>.toml` — a list of (formation × flop set × overrides) that
`solve-gen gen --manifest` walks, **skipping outputs whose config-hash already
exists** (resumable; pairs with `scripts/idle-run.sh`). The current hardcoded
`curated()` becomes `manifests/starter-8.toml`.

Flop sets are named lists in the manifest, plus one generated set:
`all-iso-flops` = the 1,755 suit-isomorphic flops, enumerated in code (tiny
function; the standard 22,100/isomorphism reduction). Tiers: `starter-8`
(in git) → `texture-25` → `all-1755` (shipped as `manifests/all-1755.toml`
for the reach-pruned *tables*, one run per formation — with the iso lookup
of [design 08](08-instant-flops.md) that tier is complete instant coverage
of all 22,100 flops; generated locally, never committed). The intermediate
`broad-95` snapshot tier lost its reason to exist and was dropped.

## Scale math & storage

Measured: ~200 KB pretty JSON per node snapshot, 3 nodes per (formation,
flop). So `broad-95` × 4 formations ≈ 95×4×3×200 KB ≈ **230 MB** — fine on
disk; the linear directory scan in `FileSolutionProvider::load` is the first
thing to break (loads *everything* to pick one spot).

- Now: nothing. Ship v2 with the same loader.
- Trigger — library > 1 GB **or** load > 2 s: write `data/solutions/index.json`
  (path → formation/flop/node/hash) at gen time, load lazily; gzip snapshots
  via `flate2` only if disk actually hurts. Not before.

Git policy: only `starter-8` outputs stay committed; bigger tiers are
regenerated locally (document the manifest command in README).

## Milestones

1. `SpotConfig` + embedded config/generator metadata + hash filenames
   (backward-compatible loader).
2. Ranges to `data/ranges/`, formations table, rake plumbing (`--rake-rate`,
   `--rake-cap`).
3. Manifests + resumable `gen --manifest`, `starter-8.toml`, flop-set tiers,
   iso-flop enumerator.
4. Curate the three new formations' ranges; solve `texture-25` for
   `srp-btn-bb` + `3bp-bb-btn` as the first breadth drop.

## Out of scope

- Solving preflop *with the postflop engine* (06 explains why); preflop has
  its own solver crate ([07](07-preflop-solver.md)).
- Limped pots, straddles, ante formats — add as formations later if drilling
  demand shows up; the schema already carries them.
- A database. Files + an index JSON scale past any realistic local library.
