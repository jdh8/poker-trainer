# poker-trainer

A post-flop-focused **GTO poker trainer** for the command line, written in Rust.
You're dealt realistic post-flop spots, you act, and the trainer scores your
decision against a solver's equilibrium strategy — reporting EV loss and the full
optimal frequency mix.

> Status: Phase 0 done (equity + board-texture drills). Phase 1 (precomputed GTO
> solutions) is live — `drill gto` covers 8 flop textures, each as both the BTN's
> c-bet decision and the BB's defend. Phase 2 (`drill range`) builds on the same
> solutions: assign an action to your whole range by strength bucket and get
> per-bucket leak stats. See the phased plan below.

## Build & run

```sh
cargo run -- drill pot-odds   # call/fold vs. break-even pot odds (Monte-Carlo equity)
cargo run -- drill texture    # classify a flop's board texture
cargo run -- drill gto        # act vs. a precomputed GTO solution (Phase 1)
cargo run -- drill range      # assign your whole range by bucket; leak stats (Phase 2)
```

The `gto` drill needs solution files; generate them with the (AGPL) solver crate:

```sh
cargo run -p solve-gen        # writes data/solutions/*.json
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
3. **Live solving (optional)** — `postflop-solver` behind `SolutionProvider` for
   custom spots, with explicit "~30 s, ~1 GB RAM" expectations.

## Licensing

The trainer crate (`poker-trainer`) is `MIT OR Apache-2.0` and **never links the
solver** — it only deserializes the JSON that `solve-gen` produces. `solve-gen`
is its own crate, licensed **AGPL-3.0** because it links `postflop-solver`
(AGPL, git-only). The `data/solutions/*.json` files are solver *output*, not a
derivative work, so they ship with the permissively-licensed trainer.

Not legal advice — AGPL's network/derivative-work terms matter if you ship this.
