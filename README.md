# poker-trainer

A post-flop-focused **GTO poker trainer** for the command line, written in Rust.
You're dealt realistic post-flop spots, you act, and the trainer scores your
decision against a solver's equilibrium strategy — reporting EV loss and the full
optimal frequency mix.

> Status: Phase 0 done (equity + board-texture drills). Phase 1 (precomputed GTO
> solutions) is in progress — see the phased plan below.

## Build & run

```sh
cargo run -- drill pot-odds   # call/fold vs. break-even pot odds (Monte-Carlo equity)
cargo run -- drill texture    # classify a flop's board texture
cargo run -- drill gto        # act vs. a precomputed GTO solution (Phase 1)
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
1. **Precomputed-range comparison (the core product)** — *v1 done.* `solve-gen`
   solves a curated library offline with `postflop-solver`, dumps per-hand
   strategy tables to `data/solutions/*.json`, and `drill gto` loads them via
   `FileSolutionProvider` and scores your action on EV loss vs. the equilibrium
   mix. Expand by adding spots/decision-nodes in `crates/solve-gen`.
2. **Range-builder mode + leak stats** — assign the action for a whole range and
   score the full strategy; track per-spot EV loss.
3. **Live solving (optional)** — `postflop-solver` behind `SolutionProvider` for
   custom spots, with explicit "~30 s, ~1 GB RAM" expectations.

## Licensing

The trainer crate (`poker-trainer`) is `MIT OR Apache-2.0` and **never links the
solver** — it only deserializes the JSON that `solve-gen` produces. `solve-gen`
is its own crate, licensed **AGPL-3.0** because it links `postflop-solver`
(AGPL, git-only). The `data/solutions/*.json` files are solver *output*, not a
derivative work, so they ship with the permissively-licensed trainer.

Not legal advice — AGPL's network/derivative-work terms matter if you ship this.
