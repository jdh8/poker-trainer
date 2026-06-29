# poker-trainer

A post-flop-focused GTO poker trainer for the command line. You're dealt realistic
spots, you act, and the trainer scores your decision against a solver's
equilibrium strategy. It's a two-crate workspace: the `poker-trainer` binary
(`MIT OR Apache-2.0`) only *deserializes* the JSON in `data/solutions/`, while the
AGPL `crates/solve-gen` is the offline generator that links `postflop-solver`.

The seam between them is `solution::SolutionProvider`: everything that needs a
strategy goes through it, so a file-backed provider and (later) a live-solving one
swap without touching the trainer. Keep that boundary clean — **the trainer must
never link the solver**; only `solve-gen` may depend on `postflop-solver`.

Regenerating the solution library is CPU-saturating; on a shared box wrap it in
[`scripts/idle-run.sh`](scripts/idle-run.sh) — see
[docs/shared-machine-data-gen.md](docs/shared-machine-data-gen.md).

After updating the codebase, please

- Format the code with `cargo fmt`.
- Run the tests with `cargo test`.
- Propose a clear and descriptive commit message.
