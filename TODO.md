# TODO

Hand-off ledger. Development halted 2026-11-07 for lack of compute. The
former worker fleet is no longer available; the single dev box (16 threads,
62 GB, an HDD bulk store for tables and solves) is the only machine left.
Everything below that says "compute" means weeks on the dev box, not hours
on a fleet.

## State as of 2026-11-07

Shipped (source of truth: parity matrix in `docs/design/00-overview.md`): P4
through P12 are all done. CLI drills (`pot-odds`, `texture`, `gto`, `range`,
`hand`, `preflop`), `table` TUI with lenses and nodelocking, `stats`,
`analyze`, `report`, `equity`, `export-range`; the solver seam
(`solve-gen gen|solve|tables|serve`, protocol v2); the reach-pruned table
store with suit-isomorphic lookup (design 08); the preflop MCCFR solver plus
32 solved rulesets (design 07); the per-ruleset R-loop converged and landed
for mtt89, mtt55, mtt34, mtt21; the value net at 1.8-3.4% pot on all five
curated formations (design 09, parked); the Pages flop lookup with the
release-asset transport (design 10). The curated all-1755 table tier and the
texture-25 / starter-8 solution library are complete.

Halted: the mtt13 R-loop (lap 2 died at 879/17,550 flops), every further
R-loop rung, the half-built cash89 line tier, the never-started hu34 tiers,
the cash chart re-calibration, and all value-net follow-ups. Two converged
MTT table stores (mtt34, mtt21) are on disk but not yet merged into the
served store. The web's preflop charts and Pages table asset are stale. The
two fleet-era `poker-worker@` units were disabled 2026-11-07 (section 4,
first item); their stride drop-ins are inert but still exist.

## How to resume

1. Read first: `CLAUDE.md`, `docs/design/00-overview.md`, this file. Skills:
   `.claude/skills/run-app/` (headless recipes),
   `.claude/skills/solution-library/` (regen recipes). Everything this ledger
   depends on is inlined below.
2. Definition of done before any commit (CI runs exactly these):
   ```sh
   cargo fmt --check
   cargo clippy --workspace --exclude solve-gen --all-targets --all-features -- -D warnings
   RUSTDOCFLAGS="-D warnings" cargo doc --workspace --exclude solve-gen --no-deps --all-features
   cargo test --workspace --exclude solve-gen --all-features
   ```
   Plus `cargo clippy -p solve-gen` when touching the solver crate. Ignored
   tests: five solver-spawning ones in the root crate (`src/tree.rs:482,569,857`,
   `src/trainer.rs:1834`, `src/analyze.rs:1074`; CLAUDE.md says three) run
   with `cargo build -p solve-gen --release && cargo test -p poker-trainer --
   --ignored` when touching `tree.rs`, `LiveSolutionProvider` or `solve-gen
   serve`; two more slow ones live in preflop-gen (`mccfr.rs:587` 10bb HU
   push/fold convergence ~1-2 min, `equity.rs:363`) and run via `cargo test -p
   preflop-gen -- --ignored`. A workspace-wide `-- --ignored` runs all seven.
3. Store precondition before any generation: `data/tables` must be the
   symlink to the bulk-store tables directory outside the repo (`readlink
   data/tables`; a fresh
   checkout without it writes tables into the repo). Expected dirs there:
   `srp-*`/`3bp-*` (curated, 1,779 jsonl each), `mtt89_*` 10, `mtt55_*` 10,
   `cash-hu55_*` 7, `cash89_*` 10 (partial, section 1b); grounded line dirs
   carry two `header-<hash8>.json` (rainbow vs non-rainbow sizing map). The
   resume gate is `solve-gen tables` itself: a flop whose
   `<flop>-<hash8>.jsonl` exists prints `Cached, skipping`; a hash that moved
   (chart or R changed) re-solves everything, so check the header names first.
4. Generation rules: never bare, always `scripts/idle-run.sh cargo run -p
   solve-gen --release -- tables --manifest manifests/<tier>.toml
   --no-save-bins`; one job at a time (a second concurrent solve does not
   pay: memory-bandwidth-bound); on the dev box add `--max-mem 44`;
   limped-pot flops need 18-26 GB. Never `--stride/--offset` on a single box
   (no other box shares the manifest). Multi-hour jobs run as `systemd-run
   --user` units, never as a session background task. Measured dev-box
   throughput: 76 s/flop on mtt34
   `r3-c` (26.7 h for 1,257 flops, 1x16t idle-run, `--max-mem 44`, 2026-10-31;
   first flop 92 s), so one 17,550-flop lap or one 10-line tier is 15-16 days.
5. Restart rule (user's, 2026-10): after the user stops a long job, do not
   auto-restart it, arm a timer or tripwire, or plan its resume; the user
   starts it. Nothing on the dev box should auto-resume: the two
   `poker-worker@` units were disabled 2026-11-07 (section 4, first item),
   but their stride drop-ins still exist and must be deleted before any
   resume.
6. Commit directly to `main`. Never commit under `data/solutions/`,
   `data/tables/`; never hand-edit generated data.

## 1. Compute-bound work

Priority order follows the MTT-first rule (the user plays MTT far more than
cash).

### 1a. Preflop R-loop calibration laps (design 07)

- **mtt13 lap 2: rerun, calibrate, land or interpolate.** Lap-1 verdict
  (2026-11-06): NOT converged. Residual |G13-R21_4| = 0.051 (OOP 0.040, IP
  0.060; bar 0.02); chart-space TV 0.162 vs 0.025 seed noise (6.5x), so
  mtt13 cannot land on mtt21's R; R compresses toward 1 at 13bb. Lap-1
  outputs are on the dev box in the lap tooling kept outside the repo
  (`mtt13-lap1/{chart,chart-g13,chart-seed2,g13_r1_table.rs,g13_r1_calibration.json,resid.py,resid.txt,chartdiff-g13.txt,chartdiff-seed2.txt,macro.txt,calib.log}`).
  Lap-2 R = R21_4 + 0.55 (G13-R21_4), step 0.028, is the staged lap-2 R
  table (kept outside the repo; md5 7bad8431, confirmed;
  `mtt13-lap2/{r_table.rs,step.py,step.txt}`); its run on the former worker
  fleet died at 879/17,550 flops and that output is lost. Order of a rerun
  matters: (1) install the staged R as
  `crates/preflop-gen/src/r_table_mtt13.rs` plus an `"mtt13"` arm in
  `icm::r_table` (`crates/preflop-gen/src/icm.rs:100-102` has arms only for
  mtt21/34/55 and falls through to mtt89's `r_table::R_TABLE`); (2) re-solve
  the mtt13 chart with it (~15 min, idle-run); (3) only then run
  `manifests/lines-mtt13.toml` (~2 weeks on the dev box) into a lap dir, otherwise
  the lap solves under mtt89's R; (4) `scripts/calibrate-r.py --prior`
  (kappa 0.005), residual vs R13_2, chart diff vs lap 1 and seed noise; (5)
  land like 3a62047 (chart to `data/preflop/mtt13/`, design 07 entry, hash
  check, DoD). If still not converged, laps 1-2 bracket: interpolate with the
  mtt21 lap-4 line model. One-box driver template: the dev-box driver script
  in a local lap worktree (the dev box's share of mtt34 lap 2, `systemd-run
  --user`, 1x16t idle-run, `--max-mem 44`); drop its `--stride 2 --offset 0`.
- **Lap driver bug (prerequisite for any rerun).** The driver's "tables done"
  line counts jsonl files and never `wait`s on worker PIDs: the mtt13 lap-2
  run exited 0 with all 8 workers dead. Add `wait $pid` + exit checks. Where
  the pieces are (none under version control): the fleet lap drivers
  (`mtt13-lap2.sh` plus every other `*-lapN.sh`) and the calibration driver
  `poker-mtt13-lap1-logs/calib/run.sh` are in the former-fleet sync (section
  4; lap 2 never reached calibration, so `poker-mtt13-lap2-logs/` has no
  `calib/`; one `calib/run.sh` per lap under the other `poker-*-logs/`);
  `resid.py` per lap dir (e.g. `mtt13-lap1/resid.py`) and `chartdiff.py`
  (top level, also under `lap4/`) are in the lap tooling kept outside the
  repo.
- **Lap tooling census** (the lap tooling kept outside the repo):
  `chartdiff.py`, `chartev.py`, `macro.py`, `lapsize.py`, `rho.py`,
  `fan.sh`, `run.sh`, `sample.sh`, `sample-old.sh`, `chartdiff-all.txt`,
  `chartev-all.txt`, `macro.txt`, `Rinf_pooled.npy`, `archive/` (hash lists),
  `{old,others,new}` + `.hashes` + `r1-sources.txt` (the df8e0ae
  reproduction), per-lap dirs `mtt{55,34,21,13}-lap*` and `lap2..lap5`.
  `scripts/` holds only `calibrate-r.py`, `collect-shards.sh`, `idle-run.sh`,
  `poker-*.service`, `publish-tables-web.sh`, `serve-web.sh`. First resume
  step: salvage `chartdiff.py`, `chartev.py`, `macro.py`, `resid.py`,
  `rho.py`, `lapsize.py` and a fixed one-box lap driver into `scripts/`, so
  the loop no longer depends on a directory outside the repo.
- **Remaining MTT rungs: mtt8, mtt5, mtt144, then mtt-hu{3..55}.** No R of
  their own; `crates/preflop-gen/src/icm.rs:96-104` falls through to mtt89's
  table. Each rung = a `lines-<id>.toml` manifest (only mtt-hu34 has one) +
  1-5 laps of ~17,550 flops + calibration. Depth moves R measurably
  (|G34-G55| 0.043, |G21-G34| 0.031, |G13-G21| 0.049), so rungs cannot be
  pooled; short rungs are push/fold-dominated and may end in one lap.
- **Cash rulesets (all 17 cash*/cash-hu*) still ground on the old
  uncalibrated charts.** The 2026-09-28 calibrated re-solve is parked on
  branch `calibrated-charts` (7a94edf) with full exports
  (`preflop-charts-calibrated/<id>/charts.jsonl`) on the bulk store. Merging moves
  every grounded cash hash and orphans the cash-hu55 (7 lines) and cash89
  stores. Agreed plan: after the MTT ladder, calibrate R once per cash
  ruleset, re-solve the chart once, rebuild cash tables once (cash-hu55 7 +
  cash89 10 + cash-hu34 6 lines, ~40k flops). Decide a stopping rule first,
  or document that cash stays on old charts permanently.
- **Nearest-deeper-rung R fallback** (`icm.rs:96`, ponytail): small code
  change (mtt13 -> mtt21 -> ...), but it changes the R un-lapped rungs solve
  under, so land only together with a re-solve.
- **Deep-rung convergence check.** Doc 07 Caveats, Convergence bullet
  (07:542-557): push cash55/89/144 beyond the 1G-hand budget if
  rarely-reached lines look noisy; no header signal tracks distance to
  equilibrium, diff against a hotter solve. ~9-13 h per rung. Never done.
- **Second-order R leak.** One R per class pools limped (trash realizes ~1.0)
  and raised (~0.67) pots, worth ~0.04; multiway R is a flat 0.96 heuristic
  per extra player (`icm.rs:77`). Fix = R keyed by (class, position, pot
  type) in `r_table` + `calibrate-r.py`, then another lap per ruleset.
- **True ICM preflop tier** (`icm_payouts` in `manifests/preflop/*.toml`,
  BB-posts-the-ante mechanic, per-seat stacks via `game.rs:84`). Machinery
  exists (Malmuth-Harville); no manifest uses it. See doc 07 §Rake & ante and
  `docs/rake-ante-survey-2026.md:139-149`. Any tree change re-solves all 32
  charts (~15 min each) and moves every grounded hash.

### 1b. Postflop table generation queues (design 08)

Current store (the bulk-store directory behind the `data/tables` symlink):
curated srp-*/3bp-* complete (1,779 jsonl each incl. 24 manifest-order
twins); mtt89 10/10, mtt55 10/10, cash-hu55 7/7 complete under current hashes
(every grounded dir carries the two post-712dc31 sizing-map headers, so the
earlier "regen of lines-{cash-hu55,cash89,mtt89} pending" note is closed).
Every `manifests/lines-*.toml` header still says "Fannable across the box
fleet (--stride/--offset ...)" and "Queue AFTER manifests/all-1755.toml":
both stale (section 5); on the dev box run unstrided, all-1755 is done.

- **cash89 tier half-built** (`manifests/lines-cash89.toml`): 3 lines at
  1755, `f-r3-f-f-f-c` at 1708, six lines at 877-878 (`f-f-f-f-c-x`,
  `f-f-f-f-c-r3-c`, `f-f-f-r3-c-f`, `f-f-r3-f-c-f`, `r2.5-f-f-f-f-c`,
  `r3-f-f-f-f-c`). The other halves were solved under foreign hashes (a stray
  `charts.jsonl` on a worker) and purged 2026-10-14. ~5,300 flops left, the
  resume gate skips done flops. Do NOT resume via the (disabled 2026-11-07)
  `poker-worker@lines-cash89` unit: its inert stride drop-in still walks only
  `--stride 4 --offset 3` (section 4). Low priority: moot if the
  cash charts get re-calibrated first (hashes move again).
- **cash-hu34 and mtt-hu34 tiers: zero tables.** Manifests exist
  (`lines-cash-hu34.toml` 6 lines, `lines-mtt-hu34.toml` 4 lines); the old
  fleet-store copies were foreign-hash and are unreachable. ~17,550 solves total;
  mtt-hu34 should get its own R first. mtt-hu34 outranks cash-hu34.
- **mtt13 (and later mtt8/mtt5) line stores** only materialise once their
  R-loops converge (1a).
- **Curated tier never got the <=2-button sizing map** (712dc31):
  `src/texture.rs:73` `specialize()` skips formations without `:` so curated
  hashes stay valid. Upgrade = drop the guard and regen all-1755 for the five
  curated formations (5 x 1,755 solves). Deliberate, not owed.
- **Stack-depth variants (40bb/200bb) and rake presets** (doc 02:83) exist
  only as schema; no manifest sets them. Unblocks `analyze`'s `Skip::Stack`
  band (`src/analyze.rs:431`; stacks bucket to the nearest library depth,
  doc 05:38) and the limped/multiway/ICM skips (doc 05:84) only via new
  curated formations.
- **Sizing sub-classes** (`src/texture.rs:35` `sizing_class == class`): split
  a class only once a reference solve shows it wants different sizing.

### 1c. Value net (design 09, parked 2026-09-28)

Checkpoint `train/value-net-offeq6.pt`; corpora in cold archive
(`valuenet{,-smoke,-offeq,-v2}.tar.zst`; nothing value-net-related remains
on the bulk store except the two extract logs); the training GPU (a
former-fleet 4090) is gone. Tooling: the corpus
exporter is the root-crate binary `src/bin/export-value-corpus.rs` (reader
`train/corpus.py`); training/eval scripts are `train/{train.py,dls.py,
offeq.py,eval.py}` with `train/pyproject.toml` + `uv.lock`. There is no
README or run recipe anywhere in the repo; doc 09 is the only description
and it is 400 lines of history. Write the recipe before reopening. Reopen
only when a new tier makes bulk generation hurt or net-backed spots become a
product want; then (4) before (5).

- (2) Low-mass reach encoding for the residual ~1% pot (log-mass / raw
  per-combo reach): an afternoon on the existing 490k labels, no generation.
- (3) Exploitability of the DLS strategy vs the full game: needs a solve-gen
  nodelock op + `train/dls.py` strategy export (~1 day) plus one full solve
  per validation flop. Do this before shipping anything under (4).
- (4) Permissive Rust depth-limited solver (product path, 1-2 weeks; tract/ort
  dependency decision; doc 00 product call, "do not start it to see if it
  works").
- (5) Grounded-tier nets: v2 corpus (206 GB uncompressed, IN_DIM 2709 with
  rake) extracted over all 32 tiers but never trained; it lives only in
  cold archive as `valuenet-v2.tar.zst` (27 GB), restore before training.
  `export-value-corpus` already reads the two-headers-per-dir grounded
  layout (fae3a8a, `src/bin/export-value-corpus.rs:858-877`); doc 09:116-118
  saying otherwise is docs lag (section 5).
- 3bp-btn-co trails at 3.37% pot on 217k labels: a labeling round there would
  be data, not method.
- Doc 09:314-317 ops (fishnet/shoginet on former-fleet hosts, worker label
  cleanup, hu34 fleet-store decision) are now moot: hosts gone. Needs a
  closing note, not action.

### 1d. Solver research notes (standing, no schedule)

- ICM postflop needs a fork of `postflop-solver` with a terminal utility
  transform (doc 06:79-90); interim stance is chip-EV postflop with
  ICM-aware preflop. Only if MTT training becomes the real use case (it is).
- Multiway postflop: out of scope until a credible open engine appears (doc
  06:92-98). Formats already generalize.
- preflop-gen MCCFR variance is the convergence ceiling (`mccfr.rs:11`,
  `:97`): named upgrade is a vectorized CFR+ backend for the 2-player
  subgame behind the same `NodeData` layout; the sequential batch merge is
  the Amdahl ceiling.

## 2. Code and features deferred (no compute)

- **04 M3: spot filters + curated-library sampling for `drill hand`.**
  `src/trainer.rs:997-1003` still bails with "needs --board". Doc 04 status
  line (04:3) says "shipped except milestone 3"; spec at doc 04:35
  (`--formation`, `--texture wet|dry|paired`, reuse of the pot-odds filters).
  Data precondition met (texture-25 for all five formations, all-1755 store).
  Would pair with multi-session spaced repetition of failed spots from
  `history.jsonl` (doc 04:89-90, `src/stats.rs` has nothing yet).
- **P10 deferred trio (doc 06:63-71):** (a) `drill hand --locks <file>`
  villain personas (`--locks` exists only on `Command::Table`,
  `src/main.rs:52-55`); (b) session-wide EV-delta lens: `serve` must retain
  the unlocked game beside the locked one (protocol + `src/tree.rs` +
  `crates/solve-gen/src/main.rs` serve loop); (c) warm-start `resolve`
  (`crates/solve-gen/src/main.rs:811`): also the named upgrade for cold
  river/off-frontier live solves (doc 08:144) and the 05 node-locked
  re-analysis for known-bad pools (doc 05:69).
- **Weighted dealing in `drill hand`.** Protocol v2 already ships per-hand
  reach `weights` on every node (design 01 status line and §payload, 01:57;
  `src/tree.rs:47-49` deserializes `pub weights: Vec<f32>` parallel to
  `hands`). Only the consumer is missing: `src/trainer.rs:1032` ignores
  `walk.play(0)?.weights` and deals uniformly over combos, so grounded
  `export-range` lines (weighted) are flattened. Fix = sample by weight in
  the trainer; no protocol change. Its ponytail comment still says "protocol
  v1 carries no range weights" (stale, update it). Related: `villain_range`
  (`trainer.rs:1393`) is unweighted and includes the bettor's checking hands.
- **`drill gto` / `drill range` snapshots don't canonicalize flops** (doc
  08:148): only the table path uses `src/iso.rs`; ~half a day, no index
  hazard.
- **Accuracy knobs not exposed** (doc 06:100-107): target exploitability and
  max iterations are not `SpotConfig` fields; compression is already default.
- **Bunching effect** (`set_bunching_effect`) available in the pinned solver
  rev, not exposed (doc 06:72-77). Low priority.
- **`table` tree-browsing on curated snapshot spots** (snapshot fallback, doc
  03:27, 03:80, 03:86; snapshot-only reports 03:67-70): snapshots are
  unit-weight grids with NaN equity in `src/table.rs:53,67`. Saved study
  sessions/bookmarks: only if navigation depth hurts (doc 03:96).
- **Solution index** (`data/solutions/index.json`, doc 02:106-112): trigger is
  library > 1 GB or load > 2 s; currently 46 MB / 381 files. Not due.
- **Pages ceiling breaker:** HTTP Range fetches straight from the release
  asset with an offsets index (doc 10:99-102); prerequisite is a curl check
  that the github.com redirect carries `Access-Control-Allow-Origin`. Needed
  to publish mtt55/34/21 beside the existing lines (see 4).
- **In-browser solver:** abandoned (user's call, 2026: the rigorous route is
  `export-range` -> solve-gen offline; the web equity explorer stays).
  `web/` has no uncommitted changes on main (it is a separate Cargo
  workspace, not a separate git repo); web footer/README's "solver never
  linked here" is true.
- **Release process:** all crates 0.1.0, no CHANGELOG, no version tags
  (`tables-web` is an asset tag), root `Cargo.toml` lacks
  `repository`/`readme`/`keywords`. `POKER_TRAINER_SOLVE_GEN` is documented
  (README:97, doc 01:21, run-app skill); a packaged release would still need
  a shipped `solve-gen` binary or install instructions, i.e. the `cargo run`
  shim ponytail in `src/solution.rs:370`. Whether to publish at all is
  undecided.
- **CI never builds solve-gen**, so the five `#[ignore]` solver tests
  (`src/tree.rs:482,569,857`, `src/trainer.rs:1834`, `src/analyze.rs:1074`)
  only run by hand (How to resume, step 2). A solve-gen CI job is the
  ceiling.

## 3. ponytail: ledger

39 `ponytail:` comments: 37 in `src crates web` plus 2 in `scripts/`
(`grep -rn "ponytail:" src crates web/src web/app.js scripts`). Deliberate
shortcuts with a named ceiling; do not "fix" unasked. Grouped:

| where | shortcut | ceiling | upgrade |
|---|---|---|---|
| `src/eval.rs:45`, `src/report.rs:288`, `src/preflop.rs:142`, `web/src/lib.rs:101` | Monte-Carlo range equity with small fixed iters/caps (web duplicates the knobs; call must stay sync) | noisy at decision boundaries | exact turn+river enumeration (990 runouts); export shared constants |
| `src/eval.rs:105` | made-hand gate handles board pairs only | pure-board hands on trips boards mis-bucketed | extend gate to trips/quads |
| `src/texture.rs:3` | classifier = suit pattern + pairing only | stats/report grouping is a 4-way split | add straightiness/high card when a consumer reads them |
| `src/texture.rs:35`, `:73` | `sizing_class == class`; sizing map grounded-only | curated tier stays 33/75 | measured sub-classes; drop `:` guard + regen (compute) |
| `src/analyze.rs:431` | one depth per formation, `STACK_BAND` | off-band hands skipped | depth-variant tiers (compute) |
| `src/trainer.rs:145` | all-in guard `pot >= 2*stack` is coarse; deep HU (cash-hu89) never hits it | short-stack HU rulesets are the case where a labeled "Raise to X" could in fact be all-in (unverified, no observed instance) | precise commitment tracking |
| `src/trainer.rs:999` | `drill hand` requires `--board` | no spot sampling | 04 M3 |
| `src/trainer.rs:1032`, `:1393` | uniform combos (comment claims "protocol v1 carries no range weights": stale, v2 ships `weights`); unweighted villain range | weighted ranges flattened; equity vs too-wide range | sample by `weights` in the trainer; bet-freq weighting |
| `src/table.rs:101`, `:140`, `:1121` | buckets by flop only; action-count guard drops combos silently; blocker "continue" vs biggest bet only | turn/river improvement not shown; small-bet blocker picture missing | classify full board; one villain node per size |
| `src/tree.rs:400` | `runouts()` on a table walk always live-solves | CPU-minutes per call | synthesize from stored turn children |
| `src/solution.rs:246`, `:370` | O(n) directory load; `cargo run` shim for solve-gen | fine for curated; no standalone distribution | board-key index; `POKER_TRAINER_SOLVE_GEN` at a packaged binary |
| `src/main.rs:536` | tables-web EV as i16 centi-bb | saturates at 327 bb (fine to 144bb) | deci-bb or f16 for deeper rulesets |
| `src/preflop.rs:371`, `:404`, `:531`, `:547` | class-level reach/line mass (no blockers, no cross-seat correlation); flat ante, equal stacks; pruned continuation can look like a flop close on the starter tier | rankings approximate; starter tier misreports rare lines | combo-level reach; per-seat commitment; regen `charts.jsonl` (~15 min/ruleset) |
| `src/bin/flop-equity.rs:8` | brute-force pair loop (~600M compares) | seconds per flop | sort-by-rank counting if run over many flops |
| `crates/preflop-gen/src/game.rs:84`, `:102`, `:266`, `:373` | uniform stacks; one squeeze size; public-state infoset merge (imperfect recall); raise-over-limpers reuses open menu | no all-in-for-less; chart nuance in squeezed/limped pots | per-seat stacks; wider menu; full-history keys (~3x memory). Any change re-solves 32 charts + moves hashes |
| `crates/preflop-gen/src/mccfr.rs:11`, `:97` | plain external sampling; sequential batch merge | sample variance; Amdahl | vectorized CFR+ 2-player backend; shard the merge |
| `crates/preflop-gen/src/equity.rs:230` | multiway terminals sample boards per visit | variance absorbed by averaging | none (cache tried, lost) |
| `crates/preflop-gen/src/icm.rs:77`, `:96` | flat 0.96 multiway R, not jointly normalized; flat fallback to mtt89's R table | see-a-flop terminals not exactly zero-sum; un-lapped rungs use mtt89's R | multiway engine (none); nearest-deeper-rung fallback + re-solve |
| `crates/solve-gen/src/main.rs:811` | `resolve` re-solves from a fresh allocation | full solve per lock edit | warm start |
| `scripts/publish-tables-web.sh:12`, `scripts/serve-web.sh:13` | full re-export each run | hours, HDD-bound | per-file mtime gate |
| `web/app.js:143`, `:521` | `hu_step` hand-off starter-tier only, coarse all-in guard; action colouring assumes <=2 non-all-in sizes | deep HU sets miss the guard; a 3-size menu needs a mid-tone | mirror trainer fix; add a tone |

## 4. Ops and infra loose ends

- **Fleet-era systemd units: disabled 2026-11-07.**
  `poker-worker@lines-mtt89.service` and `poker-worker@lines-cash89.service`
  were ENABLED (WantedBy default.target) until then; both are now disabled
  and none is running. The stride drop-ins under the user systemd dir are
  inert but still exist, delete them before any resume:
  `poker-worker@lines-mtt89.service.d/stride.conf` ran `solve-gen tables
  --manifest manifests/lines-mtt89.toml --no-save-bins --max-mem 44 --stride 4
  --offset 1 --out <served store>`; `poker-worker@lines-cash89.service.d/`
  `--stride 4 --offset 3`; a third drop-in for the dev box's residue unit
  `--stride 4 --offset 2` (residue manifest that no longer exists on main).
  The `--out` pointed at the served store, so a re-enabled unit would write
  a quarter-manifest straight into it. Never stride on a single box.
- **Merge the converged mtt34 lap-3 and mtt21 lap-4 stores into the served
  store.** `mtt34-lap3/mtt34_*` (844 GB) and `mtt21-lap4/mtt21_*` (630 GB)
  on the bulk store, 10 dirs x 1,757 files each
  (1,755 jsonl + 2 headers, e.g. `header-054c60f2.json` +
  `header-086cf100.json` in `mtt34_f-f-f-f-c-x`), header ranges match
  today's `export-range` (verified on `f-f-f-f-c-x`). Until moved into the
  served store, `drill hand --from mtt34:...` and `table --from
  mtt21:...` live-solve. Re-verify hashes against the landed
  `r_table_mtt34.rs` / `r_table_mtt21.rs` before moving: with main built,
  `cargo run -p solve-gen --release -- tables --manifest
  manifests/lines-mtt34.toml --out <bulk store>/mtt34-lap3 --no-save-bins
  --max-mem 1 | grep -c "Cached, skipping"` must print 17550 (the cached
  gate runs before the memory check, so a mismatched flop prints `SKIPPED`
  instead of solving); same for mtt21 against `<bulk store>/mtt21-lap4`.
  Then mv/rsync. No solver compute.
- **Web data is stale on all three stages; refresh together.** (a)
  `data/preflop-web/` is the 7a94edf snapshot and differs from `data/preflop/`
  for every ruleset, including the landed mtt89/55/34/21 charts, so the
  CLAUDE.md claim "hand-off exact for MTT" is currently false. Refresh MTT
  per ruleset from main (`git archive HEAD data/preflop | tar -x --transform
  's,^data/preflop,data/preflop-web,'`, mtt* only); cash stays on the 7a94edf
  copy. (b) The `tables-web` release asset (922 MB, published 2026-09-28) and
  the bulk-store `tables-web` export (09-29) carry pre-rebuild mtt89 hashes, and
  include no mtt55/34/21. Rerun `scripts/serve-web.sh --export` then
  `scripts/publish-tables-web.sh` as a `systemd-run --user` unit (hours,
  HDD-bound). (c) `web/solutions/` is a stale hand copy (151 files from
  2026-07-06 vs 381 in `data/solutions/`); `web/preflop` and `web/tables` are
  symlinks and never go stale. Rerun web/README's
  `cp ../data/solutions/*.json solutions/` or make it a symlink too.
- **Pages budget decision.** `scripts/publish-tables-web.sh` `LINES` (27
  entries, cash-hu55/mtt89/cash89 only) already overflows the 900 MB budget
  (6 listed lines missing from the asset). Adding mtt55 (and mtt34/21/13)
  means either dropping thin cash 3-bet lines (MTT first) or building the
  Range-on-release breaker (section 2). Doc 10:171-175 cash89 877-flop
  coverage gap stays until 1b fills it.
- **Data lost with the former worker fleet** (locked 2026-11-06):
  mtt21 laps 1-3 stores (superseded), mtt13 lap 1 store (472 GB; its chart and
  calibration outputs ARE on the dev box, see 1a), mtt13 lap 2's 879 flops.
  Cold archive holds only `poker-mtt21-lap1.PARTIAL-968of17550.tar.zst`; lap
  2/3 archives were empty and deleted. Ask the former fleet's admin, or
  accept the loss; nothing needed for the landed charts is missing. The
  cold-archive scripts (kept on the bulk store) now run under `set -euo
  pipefail` (remote end `bash -o pipefail`) so a dying tar fails the archive
  instead of uploading an empty one.
- **Former-fleet sync of 2026-11-07** (one dir per retired host, 1.5 GB on
  the bulk store, also in cold archive as `remote-sync-2026-11-07.tar.zst`):
  the only copies of the fleet lap drivers and per-lap calibration `run.sh`
  (see 1a). Salvage into `scripts/` if the loop resumes; otherwise deletable
  only after the user says so.
- **Local lap worktree: dirty, superseded, do not commit from it.**
  Registered `git worktree`, detached at 423304d (15 commits behind main).
  Modified: `crates/preflop-gen/src/r_table.rs` (md5 c0e4ee83 = the
  superseded mtt34 lap-2 damped R, NOT the landed R34_3) and
  `data/preflop/{mtt34,mtt55,mtt89}/{header.json,starter.jsonl}` (lap-2
  charts): grounding there gives wrong hashes. Untracked: a plain
  `manifests/lines-mtt34.toml` (older than main's f1f8355) plus four
  per-host stride slices of it, the one-box driver template (1a) and its
  8x32 fleet variant, `contention-ab.sh`, `contention-probe.sh`,
  `memprobe.c`, `abtest.log`. Nothing there is needed on main. After copying
  both driver scripts into `scripts/` or the lap tooling kept outside the
  repo: `git worktree remove --force <the worktree>`.
- **Stale merged git leftovers.** Worktree
  `.claude/worktrees/worker-sigkill-clean-stop` (branch
  `worktree-worker-sigkill-clean-stop`, b6f2250 "Worker: TERM stops, HUP
  drains", clean, ancestor of main) and branch `phase-1-gto-solutions`, both
  in `git branch --merged main`. `git worktree remove` + `git branch -d`
  both. Only `calibrated-charts` (7a94edf) is a real unmerged branch.
- **Bulk-store housekeeping (user decides; never delete unasked):**
  `mtt34-lap2` (842 GB, superseded non-converged lap),
  `tables-release` + `tables-web` + `tables-web.tar` (stale exports),
  the archive staging dir (empty), `solves/` (582 GB, 132 bins) with 7 stray bins
  (~20.6 GB) under 5 unknown config hashes 900b4438 f55543b1 8deb4b44
  906488c0 b00c5fc1 (`2ckh6d-906488c0` 10.8 GB, `thtd3c-8deb4b44` 8.2 GB,
  `td9d6h-b00c5fc1` + `ts9s6h-b00c5fc1` 0.53 GB each, `ah7c2d-900b4438`,
  `td9d6h-900b4438`, `td9d6h-f55543b1` 0.1-0.25 GB), and 2 stray solution
  files (`data/solutions/2ckh6d-906488c0-*`, `thtd3c-8deb4b44-*`) belonging
  to no manifest. Proven remove-after-upload path: the tables cold-archive
  script on the bulk store.
- **Fleet scripts have no target.** `scripts/collect-shards.sh`,
  `scripts/poker-worker@.service` (fleet-store `--out`),
  `scripts/poker-train@.service` (fleet GPU-host paths), `--stride/--offset`
  fan-out, and the claim-aware gate note in
  `docs/shared-machine-data-gen.md:162-180` are dormant until a fleet
  returns. Retire or mark historical (and see the disabled units above).
- **calibrate-r.py df8e0ae re-check: closed 2026-10-14**, not owed. The
  rebuilt store reproduced df8e0ae to 5e-5 (`{old,others,r1-sources.txt}`
  in the lap tooling kept outside the repo), and
  the 10-03 "R frozen at df8e0ae" rule was replaced by the per-ruleset R loop
  (design 07, 96d9794) that has since converged on mtt89/55/34/21.

## 5. Docs lag

- `CLAUDE.md`: "Three `#[ignore]` tests" (five in root + two in preflop-gen);
  "exact for MTT" hand-off claim (false until preflop-web refresh); "R-loop
  laps now run off-main" on the former worker fleet (gone).
- `docs/design/00-overview.md`: add a "halted 2026-11-07" status line.
- `docs/design/01-tree-protocol.md`: accurate; but the consumer comment at
  `src/trainer.rs:1032` still says "protocol v1 carries no range weights".
- `docs/design/02-solution-library.md:3-4,66-69`: says all-1755 has no
  manifest (it does) and `export-range` is unbuilt (it is: `src/main.rs:353`,
  `src/ground.rs:41`).
- `docs/design/07-preflop-solver.md`: no mtt13 entry (lap-1 verdict 0.051,
  chart-space check, staged lap-2 R, all in 1a); 07:515 still describes laps
  on the former worker fleet at 8 jobs x 64 threads (~400 flops/h, ~2 days
  per lap): that fleet is gone, the user had moved to 8x32, and the only box
  is the dev box at 76 s/flop (15-16 days per lap).
- `docs/design/08-instant-flops.md`: status line (08:3) predates the halt;
  queued tiers (08:117-119) name only cash-hu34/cash-hu55/mtt-hu34 plus
  cash89/mtt89 (mtt55/34/21/13 manifests exist); "shipped line tiers (37
  lines)" ledger (08:161) and "21 highest-mass grounded lines" on Pages
  (08:128-129) are stale; add that cash-hu34/mtt-hu34 data is unreachable.
- `docs/design/09-value-net.md`: 116-118 still says `export-value-corpus`
  cannot read the two-headers-per-dir grounded layout (fixed in fae3a8a; the
  v2 corpus was extracted with it); 314-317 and 358-360 host-specific ops
  items need closing; the bulk-store `valuenet-offeq/` paths now live
  inside `valuenet-offeq.tar.zst` in cold archive.
- `docs/design/10-pages-flop-lookup.md:19`: the hu34 tiers it lists as
  living only on the fleet store are gone.
- `docs/shared-machine-data-gen.md:94-160`: fleet section is historical.
- `manifests/lines-*.toml` headers (all eight): "Fannable across the box
  fleet (--stride/--offset ...)" and "Queue AFTER manifests/all-1755.toml"
  are stale; on the dev box run unstrided, all-1755 is complete.
- `.claude/skills/solution-library/SKILL.md:119-120`: lists the line tiers as
  `lines-{cash-hu34,cash-hu55,cash89,mtt-hu34,mtt89}.toml` "queued behind
  the formations"; mtt55/34/21/13 manifests exist and the fleet is gone.
- `README.md`: "phases 0-10" omits P11/P12; "two-crate workspace" (three);
  web reads `data/preflop-web/` not `data/preflop/`; `broad-95` tier does not
  exist; architecture table lacks `preflop`, `postflop_table`, `iso`,
  `ground`; `run_hand_drill` note at :187 still accurate.
- Standing, documented limitations (no fix planned): >=3-way preflop CFR has
  no Nash guarantee (doc 07 §Caveats); line mass ignores card removal (doc
  08:151). Cross-project: a sibling project's shrinkage plan ports the
  R-loop lessons but predates the mtt21/mtt13 refinements (t* interpolation,
  den-shift reporting).
