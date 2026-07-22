# 09 — Value net for depth-limited solving (GPU R&D)

Status: **follow-up, not scheduled.** Written 2026-07 after a GPU-feasibility
review of bulk generation. Records why GPU-porting the CFR engine is the wrong
move, and the one route where the local GPU (RTX 4070 SUPER, 12 GB) genuinely
pays. Trigger to pick this up: the line-tier queues' multi-month cost becomes
unacceptable, or we want instant solves for off-tree/off-store spots.

## Why not GPU-CFR (the reviewed-and-rejected route)

- **All compute lives in the pinned solver.** solve-gen contributes
  `allocate_memory` + one `solve()` call; the CFR kernels are inside
  `postflop-solver` (AGPL, upstream suspended Oct 2023, its GPU request —
  issue #35 — never answered). A GPU port means forking and maintaining a
  solver core, not editing solve-gen.
- **The working set doesn't fit.** A flop→turn→river game is ~9 GB in f32
  (~4.5 GB in the solver's 16-bit mode). The 12 GB card keeps ~4 GB busy with
  the desktop, and published GPU-CFR formulations are *denser* than CPU ones
  (matrix form trades memory for parallelism). Once the tree spills, every
  iteration streams over PCIe 4.0 at ~32 GB/s — slower than the CPU reading
  its own DDR5 at ~80 GB/s. The GPU then loses outright.
- **Even resident, the ceiling is modest.** GPU wins on this
  bandwidth-bound workload scale with the bandwidth ratio (~500 vs ~80 GB/s ≈
  6× theoretical, 2–3× realistic after irregular tree gather/scatter).
- **The literature doesn't beat our baseline.** GPU-CFR papers (arXiv
  2408.14778, 2605.14277) report their speedups against OpenSpiel's tabular
  Python/C++ — on small games the GPU port ran *slower* — and none benchmark
  full NLHE postflop trees against an optimized vectorized CPU solver.

What we did instead (shipped alongside this doc): 16-bit solve storage
(`tables --compress`) plus flop-level concurrency (`--stride/--offset` +
`RAYON_NUM_THREADS`) — ~20 lines for most of the wall-clock win GPU-CFR could
have offered.

## Where the GPU does pay: depth-limited solving with a learned value function

The ReBeL / GTO-Wizard-AI shape, scoped to our pipeline: stop expanding the
full turn→river tree and instead solve a **flop-only tree whose leaves call a
trained value net** V(board, pot geometry, both ranges) → per-hand
counterfactual values at each turn root.

Why this attacks the real cost: the 49 turn × 48 river expansion *is* the
9 GB and the minutes. A flop-only tree is ~2 orders of magnitude smaller —
seconds and ~100 MB per solve — which turns the 4–6-month line tiers into
days and makes off-store spots near-instant.

- **Training corpus: the store we're already generating.** Every solved flop
  in `data/tables` / the bin cache yields thousands of labeled samples
  (turn-root ranges → solver-exact counterfactual values). The all-1755 tier
  alone is millions of samples across 5+ configs. No new generation needed.
- **Net + hardware fit.** Input is two 1,326-dim range weight vectors plus
  board/pot features; an MLP/small-transformer regressor at this size trains
  comfortably on the 12 GB card. Inference batches thousands of leaves per
  solve step.
- **Licensing route mirrors preflop-gen (07).** postflop-solver has no
  leaf-value hook, so depth-limited solving needs either a small AGPL fork
  (inject leaf values) or — the cleaner precedent — an **original,
  permissive vector-CFR over flop-only trees** in the spirit of
  `crates/preflop-gen`: same DCFR math we've already written once, tiny tree,
  net at the leaves. That keeps the trainer's MIT/Apache side clean and makes
  the AGPL crate purely a corpus generator.
- **Open questions (the R&D).** Does net-at-turn hold overall exploitability
  within ~1% pot vs full solves (validate against the 1,755-flop store —
  we uniquely have exact ground truth)? River-only netting first (smaller
  accuracy risk, smaller win) or straight to turn? Range representation that
  generalizes across pot/stack geometries vs per-config nets?

Rough phasing when picked up: (a) corpus extractor over existing
tables/bins, (b) net + held-out-value eval harness, (c) permissive
depth-limited solver using the net, validated flop-by-flop against full
solves. Research-L; each phase falsifiable on its own.

This narrows doc 00's "no NN approximator" stance rather than reversing it:
the net would accelerate **our own offline generation and off-tree lookups**,
not chase datacenter solve-speed parity as a product.
