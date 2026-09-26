# 09 — Value net for depth-limited solving (GPU R&D)

Status: **phases (a)+(b) shipped** (2026-07-24) — corpus extractor
(`src/bin/export-value-corpus.rs`, one fixed-width record per stored turn
root, both invariants checked against every file) and the `train/` harness
(uv + torch MLP, equity-baseline eval) are done. **Phase (c) prototyped**
(2026-09-26, `train/dls.py`): the depth-limited flop solve works end to end
and pins the blocker — the net is only accurate at equilibrium reaches, and
CFR queries it everywhere else (see "Phase (c) result" below).
First measured result below (2026-07-24): ~2× under the equity baseline
on held-out flops; the 200-epoch run (evaluated 2026-09-26) reaches
2.7–3.4% pot on every formation and side, ~6× under baseline.
Written 2026-07 after a GPU-feasibility review of bulk generation. Records
why GPU-porting the CFR engine is the wrong move, and the one route where
the local GPU (RTX 4070 SUPER, 12 GB) genuinely pays. The trigger fired:
the line-tier queues' multi-month cost (64,935 solves, ~1% done at pickup)
is exactly the "unacceptable" case named below.

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
(now the default in `tables`) plus flop-level concurrency (`--stride/--offset` +
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

Phase (a)/(b) implementation notes (shipped): the leaf interface is the
**turn root** (stored node whose line ends with a deal token; always OOP to
act). `export-value-corpus` reconstructs both pure reach vectors by walking
line prefixes from the header ranges, takes OOP counterfactual values from
the stored mix, and rolls IP values back from the stored IP children with
card-removal-correct mixing — exact only when every child is stored (~66%
of roots; the rest keep OOP labels and mask IP, since a pruned child can
carry ~20%-pot bias). Every root is checked against two identities (stored
weights = reach × unblocked-opponent mass; both sides' reach-weighted value
averages sum to the pot — each holds to the store's 3-decimal rounding,
within ~1% of pot) and the run fails loudly on drift. Val split = flops
with fnv1a64 % 10 == 0; the eval baseline is
`cfv/pot := equity`. `train/` is a uv project (torch MLP, 2707→3×1024→2652,
reach-weighted masked MSE); corpus and checkpoints stay out of git.

First measured result (2026-07-24, all-1755 corpus: 3.34 M turn roots →
3.02 M train / 0.32 M val on 175 held-out flops; 20 epochs ≈ 2 h on the
4070): reach-weighted MAE of cfv/pot on held-out flops is **7.5–9.4% pot
on the srp tiers and 12.4–16.7% on the 3bp tiers**, vs 16.9–19.7% for the
equity baseline — roughly 2× under baseline, still far from the ~1% solve
floor. Val loss was still falling at epoch 20, so the near-term levers are
training time and input encoding (suit-iso augmentation, per-config
heads), not more data.

Second result (`train/value-net-200.pt`, 200 epochs with cosine decay on
the same corpus, evaluated 2026-09-26): **2.7–3.4% pot MAE on every
formation and side** (srp oop 2.8–2.9%, ip 2.8–3.2%; 3bp oop 2.7–2.8%, ip
2.7–3.4%), i.e. training time alone closed most of the srp/3bp gap and
took the net ~6× under the equity baseline. Still ~3× the ~1%-pot solve
floor. Good enough to justify phase (c) prototyping; not yet good enough
to replace solves. The corpus is still the curated all-1755 store only;
the grounded tiers (cash-hu34/hu55, mtt-hu34, cash89, mtt89 — all complete
2026-09) carry two headers per line dir (rainbow vs non-rainbow sizing
map), which `export-value-corpus` does not yet accept.

## Phase (c) result (2026-09-26): the leaf is right, the distribution is wrong

`train/dls.py` is the prototype: vector DCFR over all 1326 combos per side on
the flop street only, tree shape from `solve-gen tree` (the solver's own
`ActionTree`, so sizes/all-in/merge rules match the store), fold leaves exact,
the all-in leaf from `flop-equity` (exact 1326×1326 showdown matrix, ~1.5 s
per flop), and every street-closing node batched through the net for both
sides' turn-root values under the current reaches (49 turn cards × 17 deal
nodes on an srp tree — one forward pass per iteration, 200 iterations in
~8 s on the 4070). Validation is against the stored full solve of the same
flop: reach-weighted root EV MAE in % pot and L1 strategy distance.

Six held-out srp-btn-bb flops, `value-net-200.pt`, 200 iterations:

| leaf inputs | root EV MAE | root strategy L1 | deeper-node L1 |
|---|---|---|---|
| net at the **solver's equilibrium reaches** (frozen, diagnostic) | **1.36% pot** | 0.26 | 0.29 |
| net at the **live CFR reaches** (the real thing) | **13.7% pot** | 0.67 | 0.51 |

The frozen row proves the pipeline: through `dls.py`'s own feature path the
net sits at 2.1–3.3% pot on those flops' stored turn roots, and a flop solve
over those leaves lands within the net's own error of the solver (per-action
root EVs within 0.1–0.4 bb; the residual strategy L1 is the solver's mixing
at near-indifferent hands, which no 3%-pot value can pin). The live row is
the known depth-limited-solving failure: the corpus holds turn roots **only
at equilibrium reaches**, so the reach vectors CFR feeds the net during its
early (uniform-strategy) and exploratory iterations are out of distribution,
the net returns nonsense there (root EVs off by 3–6 bb), and regret matching
exploits exactly those errors. More epochs or a bigger equilibrium corpus
cannot fix this; the training distribution must cover the reaches the
solver visits.

Next lever (ReBeL's answer, scoped to our pipeline): **label off-equilibrium
turn roots by solving turn-rooted subgames.** postflop-solver takes
per-combo weighted ranges and `initial_state: Turn`, so any (board4, pot,
reach pair) the DLS visits is an exact label after a turn+river solve
(~100 MB, seconds, not the 9 GB flop game). Sample the reach pairs from
`dls.py`'s own iterations (log them per deal node per iteration), solve them
under `idle-run.sh` on the now-idle fleet, append to the corpus, retrain,
re-run this table. The table above is the acceptance test: the live row has
to approach the frozen row. Until then the net serves equilibrium-reach
lookups only — which is what the trainer's off-tree drills actually need,
and is why the corpus-side work (grounded tiers, rake inputs) still pays.

**Labeler (shipped 2026-09-26).** `dls.py --log-reaches` writes one request
per deal node every k-th iteration (a random turn card each; empty-side
reaches skipped); `solve-gen turn-solve` reads them on stdin, builds the
turn-rooted game with `Range::from_raw_data` (combo order permuted from the
corpus's `hi*(hi-1)/2+lo` to the solver's `lo*(101-lo)/2+hi-1`), solves to
0.5% pot and echoes both sides' `expected_values` in pot units;
`offeq.py check` scores the net on them and `pack` makes a corpus shard.
Cost: **0.1 s per label** on the 8-core box (turn subgame ≈ 1/49 of the
flop game), so the off-equilibrium corpus is hours, not months. Round 1:
60 non-validation srp-btn-bb flops, stride 10 → 11,797 labels in 18 min,
on which `value-net-200.pt` measures 14–16% pot (OOP) / 18–20% (IP) — the
distribution gap, versus 2.9% at equilibrium. The labeler agrees with the
store to 0.29% pot when fed the store's own equilibrium reaches (441 turn
roots of one flop), so the labels are sound.

**Round 1 fine-tune (2026-09-26).** 11.8k labels are 1.6% of the srp-btn-bb
shard: mixed in for 5 epochs they changed nothing (live root EV 13.7 →
12.4% pot). Trained on alone for 40 epochs the net fits them to 5–9% pot,
generalizes to other flops' off-equilibrium labels at 11–15%, and the live
table improves to **8.1% pot** (from 13.7) while the equilibrium eval
regresses to 9.3% (catastrophic forgetting, expected with no equilibrium
data in the mix). Error scales with reach mass: labels where one side has
under one combo of mass sit at 35% pot, mass ≥ 80 at 8–10%. Reading: right
direction, two orders of magnitude too little data — the equilibrium corpus
is 3M samples, and CFR's reach space is far larger than the equilibrium
manifold. Round 2 (`/srv/var/poker/valuenet-offeq/round2.sh`, 300 flops ×
every 4th iteration ≈ 60k labels, mixed with the equilibrium shard) is the
scaling test; if the live row keeps tracking label count, the fleet turns
the labeler loose (a million labels is a day on one 8-core box).

**Round 2 (2026-09-26, fleet).** 300 train flops × every 4th iteration →
147k requests, 145k labels; the remaining 95k after the local head were
sharded over dl02 (two processes), jdh8-22, jdh8-24 and zoo under idle-run
at ~40 labels/s combined (40 min). Fine-tune of `value-net-200` on the
srp-btn-bb equilibrium shard + both off-equilibrium shards, 10 epochs:

| labels in the fine-tune | live root EV MAE (6 val flops) | fit on own off-eq labels | off-eq labels, unseen flops |
|---|---|---|---|
| 0 (`value-net-200`) | 13.7% pot | 15–21% | 15–19% |
| 12k (round 1, shard only) | 8.1% | 5–9% | 11–15% |
| 157k (round 2, mixed) | **6.5%** | 9–14% | 12–15% |

Equilibrium eval stays at 3.0/3.2% on srp-btn-bb. Progress is sublinear in
labels and the training loss says why: the net still misses its **own**
off-equilibrium training samples by ~10% pot, so the fit is optimization- or
representation-bound before it is data-bound. Order of attack: (1) longer /
hotter training on the same data (`ft3`, same acceptance tail); (2) input
encoding for spiky, low-mass reaches (error at min-side mass < 1 combo is
35% pot vs 8–10% at mass ≥ 80 — log-mass features, or per-combo reach
straight in without L1 normalisation); (3) only then more labels. The
labeler is sound and cheap, so (3) is never the bottleneck again.

This narrows doc 00's "no NN approximator" stance rather than reversing it:
the net would accelerate **our own offline generation and off-tree lookups**,
not chase datacenter solve-speed parity as a product.
