# poker-trainer

A post-flop-focused **GTO poker trainer** for the command line, written in Rust.
You're dealt realistic post-flop spots, you act, and the trainer scores your
decision against a solver's equilibrium strategy — reporting EV loss and the full
optimal frequency mix.

> Status: phases 0–10 are all shipped — precomputed + live solving, resident
> tree sessions (`solve-gen serve`), full-hand drills with persistent stats,
> the formations library, the study browser (lenses, runouts, blockers),
> aggregate reports, hand-history analysis, and nodelocking. The parity matrix
> in [docs/design/00-overview.md](docs/design/00-overview.md) is the status
> source of truth. Still open: spot filters + curated-library sampling for
> `drill hand`, a `broad-95` manifest tier, and the P10 deferred trio
> (saved-lock villain personas, session-wide EV-delta, warm-start re-solve).

## Build & run

```sh
cargo run -- drill pot-odds   # call/fold vs. break-even pot odds (Monte-Carlo equity)
cargo run -- drill texture    # classify a flop's board texture
cargo run -- drill gto        # act vs. a precomputed GTO solution (Phase 1)
cargo run -- drill range      # assign your whole range by bucket; leak stats (Phase 2)
cargo run -- drill hand --board Td9d6h   # play full hands vs. the GTO villain (Phase 5)
cargo run -- drill preflop    # open/defend/3-bet decisions scored vs. chart files (Phase 6)
cargo run -- table            # browse a solved spot's strategy as a 13×13 grid
cargo run -- stats            # leak report over your recorded drill history
cargo run -- report           # aggregate flop report over the library (Phase 8)
cargo run -- analyze hh/*.txt # score your real hands vs. equilibrium (Phase 9)
```

The `gto` drill needs solution files; generate them with the (AGPL) solver
crate, which walks a **manifest** of (formation × flop set) entries and skips
anything already solved (resumable — pair long runs with
[`scripts/idle-run.sh`](scripts/idle-run.sh)):

```sh
cargo run -p solve-gen        # walks manifests/starter-8.toml into data/solutions/
cargo run -p solve-gen --release -- gen --manifest manifests/texture-25.toml  # first breadth tier
```

### Live solving a custom spot (Phase 3)

Pass `--board` to `drill gto`/`drill range`/`drill hand` to solve any flop on
demand instead of drilling a curated one. It's **CPU-saturating and takes tens
of seconds to a few minutes** (the wide default ranges run ~4 min on an 8-core
box) and ~1 GB RAM. The result is cached in `data/solutions/`, keyed by a hash
of the full config, and reused (and so also joins the random pool of plain
`drill gto`/`range`).

```sh
cargo run -- drill gto --board 7c5d2h      # solve this flop, then drill it
cargo run -- drill range --board Td9d6h    # same, range-builder mode
```

`--formation` picks the preflop story — seats, default pot/stacks, and the
ranges read from `data/ranges/<formation>/{oop,ip}.txt` (edit those files to
taste; they're plain solver range strings). Everything else can be overridden
per flag; a changed config gets its own cache entry instead of overwriting the
curated one:

```sh
cargo run -- drill hand --board Td9d6h --formation 3bp-bb-btn   # 3-bet pot, 18bb
cargo run -- drill gto --board Td9d6h \
  --oop "22+,A2s+,..." --ip "22+,A2s+,..." \
  --sizes "33%, 75%" --turn-sizes "50%" --stack 100 --pot 6 \
  --rake-rate 0.05 --rake-cap 3
```

Formations: `srp-btn-bb`, `srp-co-bb`, `srp-sb-bb` (single-raised) and
`3bp-bb-btn`, `3bp-btn-co` (3-bet pots).

The trainer never links the solver: `--board` shells out to the `solve-gen`
binary. In-tree it falls back to `cargo run -p solve-gen`; set
`POKER_TRAINER_SOLVE_GEN` to a prebuilt `solve-gen` binary to skip that.

### Browse a solution as a strategy table

`cargo run -- table` opens a GTO-Wizard-style **13×13 grid**: every starting hand
colored by its equilibrium action mix (red bet, green check/call, blue fold),
out-of-range hands left blank. Move the cursor with the arrows or `hjkl`, cycle
through solved nodes with `[`/`]`, and read the focused hand's exact mix in the
side panel; `q` quits.

Pass `--board` (and the same `--oop/--ip/…` knobs) to live-solve a flop and
browse the **whole game tree** instead: the solved game stays resident in a
`solve-gen serve` subprocess (Phase 4), so any line and any runout is one
keypress away. Number keys take the numbered actions, `u` goes one step up,
`r` back to the root, and turn/river cards are picked from a 13×4 card grid;
the line so far renders as a breadcrumb.

```sh
cargo run -- table                  # browse the curated library (snapshots)
cargo run -- table --board Td9d6h   # live-solve this flop, then walk its tree
```

## Architecture

A two-crate workspace — the AGPL solver lives in `crates/solve-gen` (see
Licensing); the trainer (lib + bin) is organized into modules:

| module       | responsibility                                              |
|--------------|-------------------------------------------------------------|
| `board`      | community cards (flop/turn/river)                           |
| `range`      | weighted hand ranges + `"22+, AKs"` parsing                 |
| `eval`       | hand evaluation & equity (wraps `rs-poker` / `pokers`)      |
| `texture`    | objective flop-texture classification                       |
| `solution`   | **`SolutionProvider` trait** — where GTO answers come from  |
| `trainer`    | the drill loops + scoring                                   |
| `tree`       | `TreeSession` — walk a live-solved game tree over stdio     |
| `stats`      | persistent decision history (JSONL) + the leak aggregator   |
| `report`     | aggregate flop reports + range-vs-range `equity`            |
| `analyze`    | hand-history import → EV-loss leak report                   |
| `table`      | the GTO-Wizard-style 13×13 strategy grid (TUI)              |

The key seam is `solution::SolutionProvider`. Everything that needs a strategy
goes through it, so a **file-backed** provider (precomputed sims) and, later, a
**live-solving** provider can be swapped without touching the trainer.

## Phased plan

0. **Equity & board-texture drills** — *done.* Pure `rs_poker`, no solver:
   `drill pot-odds` (call/fold vs. Monte-Carlo equity) and `drill texture`.
1. **Precomputed-range comparison (the core product)** — *done.* `solve-gen`
   solves a curated library offline with `postflop-solver`, dumps per-hand
   strategy tables to `data/solutions/<flop>-{ip,oop}.json`, and `drill gto`
   loads them via `FileSolutionProvider` and scores your action on EV loss vs.
   the equilibrium mix. Each solved flop yields the BTN's c-bet decision (a
   real size-mix: check / 33% / 75%) plus a BB defend node facing *each* c-bet
   size, across 8 textures. Grow it by adding flops/lines in `crates/solve-gen`
   (more positions, bet sizes, or turn/river nodes).
2. **Range-builder mode + leak stats** — *done.* `drill range` picks one solved
   spot, buckets its whole range by made-hand strength (value / overpair /
   top pair / pair / draw / air), lets you assign one action per bucket, then
   scores the full strategy: combo-weighted EV loss and a per-bucket leak
   report. Each big-enough bucket is split (`▲`/`▽`) at its median equity vs the
   villain's range — taken from the opposite-position node on the same board — so
   a strong and a weak top pair land in different slices you score separately.
3. **Live solving (optional)** — *done.* `drill gto`/`drill range --board <flop>`
   live-solves a custom spot through `LiveSolutionProvider`, which shells out to
   the `solve-gen` binary (the only thing that links `postflop-solver`), caches
   the result in `data/solutions/`, then drills it. `--oop/--ip/--sizes/--stack/--pot`
   forward the full postflop game config to the solver; the trainer just passes
   the strings through. A solve is CPU-saturating — tens of seconds to a few
   minutes (range/hardware-dependent) and ~1 GB RAM. (Multi-position presets and
   preflop/multiway modeling stay out of scope — this exposes the postflop knobs
   solve-gen already has.)
4. **Tree sessions** — *done.* `solve-gen serve` keeps a solved game
   resident and answers node queries over line-delimited JSON on stdio
   ([design 01](docs/design/01-tree-protocol.md)); the trainer's `TreeSession`
   drives it, and `table --board` walks the full tree (any line, any runout).
   Solved games are cached by config hash (solver-native saves), and the
   `lock`/`resolve` ops power nodelocking (Phase 10).
5. **Full-hand drills + persistent stats** — *done (core).* `drill hand
   --board <flop>` plays whole hands (flop→river) on a tree session: villain
   is dealt a hidden hand from its range and plays the solved mix *for that
   hand*, runouts deal from the unblocked deck, and your decisions are scored
   on EV loss but only revealed in the end-of-hand replay. Every scored
   decision in `drill gto`/`range`/`hand` appends a JSONL record to
   `$XDG_DATA_HOME/poker-trainer/history.jsonl`; `stats [--by
   formation|street|texture|bucket] [--last N]` reports avg EV loss, accuracy,
   blunder rate, and a trend, worst groups first
   ([design 04](docs/design/04-training-mode.md)). Remaining: spot filters +
   curated-library sampling (unblocked now that Phase 6 landed;
   `run_hand_drill` in `src/trainer.rs` still requires `--board`).
6. **Library v2** — *done.* One `SpotConfig` struct is the CLI's
   resolved knobs, the serve request body, the cache key (stable FNV-1a hash →
   `<flop>-<hash8>-<node>.json`, so custom solves never clobber curated
   files), and the provenance embedded in every snapshot (old files keep
   parsing). Ranges live in `data/ranges/<formation>/{oop,ip}.txt` with
   hand-curated charts for five formations (BTN/CO/SB single-raised pots and
   two 3-bet pots); rake and per-street bet sizes plumb through end to end.
   `solve-gen gen --manifest manifests/<name>.toml` walks (formation × flop
   set × overrides) lists resumably — `starter-8` is the committed library,
   `texture-25` the first regenerate-locally tier (solved locally, kept out of
   git per policy above), `all-iso-flops` the enumerated 1,755-flop ceiling
   ([design 02](docs/design/02-solution-library.md)).
7. **Commercial parity** — designed in [docs/design/](docs/design/00-overview.md).
   Phases 7–8 *done*: study browser v2 (lenses, bucket filter, runouts) and the
   aggregate `report` + range-vs-range `equity` tools. Phase 9 *done*:
   `analyze <hh files…>` imports PokerStars/GGPoker hand histories, matches
   them onto library formations, replays each hand through a tree session
   (`--solve-budget` caps the solving; most frequent spots first), and reports
   EV-loss leaks, a blunder list with a `table --line` replay command per
   entry, and `--jsonl` export ([design 05](docs/design/05-analyze.md)).
   Phase 10 *done*: nodelocking — the `table` lock editor (`L`), presets
   (overfold, never-raise), saved lock files (`--locks`), and an EV-delta lens
   after re-solve. Deferred: saved locks as drill villain personas,
   session-wide EV-delta baseline, warm-start re-solve
   ([design 06](docs/design/06-solver-capabilities.md)).

## Licensing

The trainer crate (`poker-trainer`) is `MIT OR Apache-2.0` and **never links the
solver** — it only deserializes the JSON that `solve-gen` produces. `solve-gen`
is its own crate, licensed **AGPL-3.0** because it links `postflop-solver`
(AGPL, git-only). The `data/solutions/*.json` files are solver *output*, not a
derivative work, so they ship with the permissively-licensed trainer.

Not legal advice — AGPL's network/derivative-work terms matter if you ship this.
