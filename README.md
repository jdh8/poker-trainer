# poker-trainer

A post-flop-focused **GTO poker trainer** for the command line, written in Rust.
You're dealt realistic post-flop spots, you act, and the trainer scores your
decision against a solver's equilibrium strategy — reporting EV loss and the full
optimal frequency mix.

> Status: Phase 0 done (equity + board-texture drills). Phase 1 (precomputed GTO
> solutions) is live — `drill gto` covers 8 flop textures, each as both the BTN's
> c-bet decision and the BB's defend. Phase 2 (`drill range`) builds on the same
> solutions: assign an action to your whole range by strength bucket and get
> per-bucket leak stats. Phase 3 adds on-demand **live solving** of any spot you
> pass via `--board`. See the phased plan below.

## Build & run

```sh
cargo run -- drill pot-odds   # call/fold vs. break-even pot odds (Monte-Carlo equity)
cargo run -- drill texture    # classify a flop's board texture
cargo run -- drill gto        # act vs. a precomputed GTO solution (Phase 1)
cargo run -- drill range      # assign your whole range by bucket; leak stats (Phase 2)
cargo run -- table            # browse a solved spot's strategy as a 13×13 grid
```

The `gto` drill needs solution files; generate them with the (AGPL) solver crate:

```sh
cargo run -p solve-gen        # writes the curated library to data/solutions/*.json
```

### Live solving a custom spot (Phase 3)

Pass `--board` to `drill gto`/`drill range` to solve any flop on demand instead
of drilling a curated one. It's **CPU-saturating and takes tens of seconds to a
few minutes** (the wide default ranges run ~4 min on an 8-core box) and ~1 GB
RAM. The result is cached in `data/solutions/` and reused (and so also joins the
random pool of plain `drill gto`/`range`).

```sh
cargo run -- drill gto --board 7c5d2h      # solve this flop, then drill it
cargo run -- drill range --board Td9d6h    # same, range-builder mode
```

Optional overrides (forwarded straight to the solver; any of them forces a
re-solve even if the flop is cached):

```sh
cargo run -- drill gto --board Td9d6h \
  --oop "22+,A2s+,..." --ip "22+,A2s+,..." \
  --sizes "33%, 75%" --stack 100 --pot 6
```

The trainer never links the solver: `--board` shells out to the `solve-gen`
binary. In-tree it falls back to `cargo run -p solve-gen`; set
`POKER_TRAINER_SOLVE_GEN` to a prebuilt `solve-gen` binary to skip that.

### Browse a solution as a strategy table

`cargo run -- table` opens a GTO-Wizard-style **13×13 grid**: every starting hand
colored by its equilibrium action mix (red bet, green check/call, blue fold),
out-of-range hands left blank. Move the cursor with the arrows or `hjkl`, cycle
through solved nodes with `[`/`]`, and read the focused hand's exact mix in the
side panel; `q` quits. Pass `--board` (and the same `--oop/--ip/…` knobs) to
live-solve a flop and browse it instead of a curated one.

```sh
cargo run -- table                  # browse the curated library
cargo run -- table --board Td9d6h   # live-solve this flop, then browse
```

## Architecture

Single binary crate for now, organized into modules:

| module       | responsibility                                              |
|--------------|-------------------------------------------------------------|
| `board`      | community cards (flop/turn/river)                           |
| `range`      | weighted hand ranges + `"22+, AKs"` parsing                 |
| `eval`       | hand evaluation & equity (wraps `rs-poker` / `pokers`)      |
| `solution`   | **`SolutionProvider` trait** — where GTO answers come from  |
| `trainer`    | the drill loop + scoring                                    |
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

## Licensing

The trainer crate (`poker-trainer`) is `MIT OR Apache-2.0` and **never links the
solver** — it only deserializes the JSON that `solve-gen` produces. `solve-gen`
is its own crate, licensed **AGPL-3.0** because it links `postflop-solver`
(AGPL, git-only). The `data/solutions/*.json` files are solver *output*, not a
derivative work, so they ship with the permissively-licensed trainer.

Not legal advice — AGPL's network/derivative-work terms matter if you ship this.
