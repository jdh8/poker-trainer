# poker-trainer

A post-flop GTO poker trainer for the command line: you're dealt realistic
spots, you act, the trainer scores you against a solver's equilibrium.
Three-crate workspace: root `poker-trainer` (MIT OR Apache-2.0, lib + bin),
`crates/solve-gen` (AGPL-3.0-or-later — the only crate that links
`postflop-solver`, pinned to rev `9d1509f`), and `crates/preflop-gen`
(MIT OR Apache-2.0 — original preflop MCCFR, no solver link,
[design 07](docs/design/07-preflop-solver.md)).

## Hard rules

- **NEVER** add `postflop-solver` (or anything depending on it) to the root
  crate. Only `crates/solve-gen` links the solver. The test
  `trainer_never_links_the_solver` in `src/solution.rs` enforces this: if it
  fails, remove the dependency you added — do not touch the test.
- The solver is reached only across a **process boundary**:
  `solution::SolutionProvider` (file-backed snapshots, or a shell-out for
  one-shot solves) and `tree::TreeSession` driving `solve-gen serve` over
  line-delimited JSON stdio, protocol v2
  ([design 01](docs/design/01-tree-protocol.md)).
- **NEVER** commit new files under `data/solutions/` or `data/tables/`
  (gitignored by design — only the starter-8 solution tier is tracked; tables
  are never committed) and **never** `git add -f` there.
- **NEVER** hand-edit `data/solutions/*.json`, `data/tables/**`, or
  `data/preflop/**`: generated output. Regenerate instead (solution-library
  skill). Unlike solutions, the preflop starter tiers ARE committed — commit a
  regen only when the manifest or preflop-gen deliberately changed.
- **NEVER** run solver generation bare on this shared machine — wrap it in
  `scripts/idle-run.sh` (solution-library skill).
- **NEVER** run `table` or a bare `drill` in a headless session — they block
  on a TTY. The run-app skill has piped-stdin recipes for every command.
- Status claims in README/design docs can lag. The source of truth for what's
  shipped is the parity matrix in
  [docs/design/00-overview.md](docs/design/00-overview.md).
- Solo repo: commit directly to `main` with a clear, descriptive message. The
  definition of done below **must** pass first.

## Definition of done

CI (`.github/workflows/rust.yml`) runs exactly these four on
Ubuntu/macOS/Windows; all must pass before committing:

```sh
cargo fmt --check          # fix with: cargo fmt
cargo clippy --workspace --exclude solve-gen --all-targets --all-features -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --exclude solve-gen --no-deps --all-features
cargo test --workspace --exclude solve-gen --all-features
```

(`--exclude solve-gen` because building the AGPL solver is too heavy for CI;
gate solve-gen locally with `cargo clippy -p solve-gen` when you touch it.)

Three `#[ignore]` tests spawn a real solve (~2 s with a warm solve cache,
minutes cold): run `cargo test -- --ignored` only when touching `tree.rs`,
`LiveSolutionProvider`, or `solve-gen serve`.

## Layout

| path | what it is |
|---|---|
| `src/main.rs` | clap CLI: `drill {pot-odds,texture,gto,range,hand,preflop} \| table \| stats \| report \| analyze \| equity` |
| `src/solution.rs` | **the seam**: `SolutionProvider` trait, `SpotConfig`, `FORMATIONS` |
| `src/tree.rs` | `TreeSession` ↔ `solve-gen serve` (live tree walking) |
| `src/trainer.rs` | all drill loops + EV-loss scoring |
| `src/table.rs` | ratatui 13×13 grid TUI: browser, lenses, lock editor |
| `src/stats.rs` | JSONL decision history + leak aggregator |
| `src/analyze.rs` | PokerStars/GGPoker hand-history import → leak report |
| `src/report.rs` | aggregate flop reports + range-vs-range `equity` |
| `src/eval.rs` | Monte-Carlo equity + made-hand buckets (rs_poker) |
| `src/texture.rs` | objective flop-texture classification |
| `src/board.rs`, `src/range.rs` | **intentional stubs — do not flesh out** |
| `src/preflop.rs` | preflop chart format + loader (the seam `preflop-gen` writes to) |
| `src/postflop_table.rs` | reach-pruned postflop table format + loader (the seam `solve-gen tables` writes to); `TreeWalk`/`TableWalk` live in `src/tree.rs` |
| `crates/solve-gen/src/main.rs` | single-file AGPL generator: `gen \| solve \| tables \| serve` |
| `crates/preflop-gen/` | permissive preflop MCCFR generator (design 07) |
| `web/` | wasm catalog of the pure examples — **own workspace**, not a member; `cargo test` there runs natively; deployed by `pages.yml` |
| `tests/` | fixtures only; all unit tests are colocated in `src/` |

## Data

- `data/ranges/<formation>/{oop,ip}.txt` — one solver range string per file.
  Formations: `srp-btn-bb`, `srp-co-bb`, `srp-sb-bb`, `3bp-bb-btn`,
  `3bp-btn-co`.
- `data/solutions/<flop>-<confighash8>-<node>.json`, node ∈ `ip | oop-33 |
  oop-75`. Gotcha: the filename flop keeps the manifest's card order, but the
  `board` array inside is solver-sorted — sort both before comparing (see
  `flop_key` in `src/solution.rs`).
- `manifests/*.toml` — resumable (formation × flop set) generation lists.
  `starter-8` is committed; `texture-25` and larger regenerate locally.
- `manifests/preflop/<id>.toml` — one preflop ruleset (seats, blinds, ante,
  raise menus, optional ICM payouts) per file; solved by `preflop-gen` into
  `data/preflop/<id>/` (design 07).
- `data/preflop/<ruleset>/{header.json,starter.jsonl}` — committed solved
  preflop charts (path-addressed nodes); `charts.jsonl` (full export) is
  gitignored and regenerates via `preflop-gen gen` (~15 min per ruleset,
  idle-run it).
- `data/tables/<formation>/{header-<hash8>.json,<flop>-<hash8>.jsonl}` —
  reach-pruned postflop tables (never committed, gitignored): flop+turn
  decision nodes stored line-addressed, river/off-path live-solves. `drill
  hand` / `table --board` prefer a table when present, else the live tree.
  Generation solves one game per flop (~9 GB RSS each) — **never run bare**,
  wrap in `scripts/idle-run.sh`: `scripts/idle-run.sh cargo run -p solve-gen
  --release -- tables --manifest manifests/texture-25.toml`.

## Conventions

- `ponytail:` comments are the only TODO convention here (no TODO/FIXME):
  deliberate shortcuts with a named ceiling and upgrade path. Don't "fix" them
  unasked. List: `grep -rn "ponytail:" src crates`.
- Tests live in colocated `#[cfg(test)]` modules, not `tests/`.
- The root crate has few dependencies on purpose; adding one is a decision,
  not a default.
- A live solve is CPU-minutes and ~1 GB RAM. Never assume it's fast.

## Pointers

- Roadmap + what's shipped: [docs/design/00-overview.md](docs/design/00-overview.md)
- `serve` JSON protocol: [docs/design/01-tree-protocol.md](docs/design/01-tree-protocol.md)
- Solver capabilities, nodelocking, ICM/multiway stances: [docs/design/06-solver-capabilities.md](docs/design/06-solver-capabilities.md)
- Polite data-gen on this shared box: [docs/shared-machine-data-gen.md](docs/shared-machine-data-gen.md)
- Running/verifying the app headlessly: `.claude/skills/run-app/`
- Regenerating or extending the library: `.claude/skills/solution-library/`
