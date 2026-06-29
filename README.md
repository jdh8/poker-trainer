# poker-trainer

A post-flop-focused **GTO poker trainer** for the command line, written in Rust.
You're dealt realistic post-flop spots, you act, and the trainer scores your
decision against a solver's equilibrium strategy — reporting EV loss and the full
optimal frequency mix.

> Status: initial scaffold. It builds and runs (`drill` prints a stub); the
> modules below are stubbed with `TODO`s and notes on which crate fills each one.

## Build & run

```sh
cargo run -- drill
```

Initial build depends only on `clap`. Add the poker crates as you reach each
phase (see `Cargo.toml`).

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

0. **Equity & board-texture drills** — pure `rs-poker`/`pokers`, no solver. Ships
   value immediately with zero licensing complexity.
1. **Precomputed-range comparison (the core product)** — solve a curated library
   of common spots offline, serialize them, load via `FileSolutionProvider`, and
   build the scoring loop. Most of the value lands here.
2. **Range-builder mode + leak stats** — assign the action for a whole range and
   score the full strategy; track per-spot EV loss.
3. **Live solving (optional)** — `postflop-solver` behind `SolutionProvider` for
   custom spots, with explicit "~30 s, ~1 GB RAM" expectations.

## Licensing note (read before phase 3)

This scaffold is `MIT OR Apache-2.0`. That's fine while nothing GTO-specific is
linked. `postflop-solver` — the one serious open-source Rust post-flop solver —
is **AGPL-3.0** and **git-only**. When you add it:

- Put it in its own crate (e.g. `crates/trainer-solver`) so the AGPL boundary is
  explicit and it doesn't block publishing the rest, and
- if you ever distribute or host a closed-source build, prefer **importing
  precomputed sim files** (your code isn't a derivative of the solver) or
  **process-isolating** the solver, rather than statically linking it.

Not legal advice — AGPL's network/derivative-work terms matter if you ship this.
