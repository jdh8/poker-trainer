---
name: solution-library
description: >-
  Regenerate or extend the GTO solution library: run a manifest through
  solve-gen politely (idle-run.sh), add a new formation or breadth tier,
  verify the outputs, and keep regenerated JSON out of git. Use for any
  change under data/, manifests/, or the FORMATIONS table in
  src/solution.rs.
---

# The solution library

One solve produces **3 snapshot files** per (formation, flop):
`data/solutions/<flop>-<confighash8>-<node>.json` with node ∈ `ip`, `oop-33`,
`oop-75` (one OOP defend node per size in `flop_sizes "33%, 75%"`).
`<confighash8>` is an FNV-1a hash of the canonical `SpotConfig` JSON —
**changing any `SpotConfig` default changes every filename**; old files must
keep parsing (invariant 2 in [design 00](../../../docs/design/00-overview.md)).
Known hashes: `d728de43` = srp-btn-bb defaults, `289b7689` = 3bp-bb-btn.
Gotcha: the filename keeps the manifest's card order (`td9d6h`), the `board`
array inside is solver-sorted (`["6h","9d","Td"]`) — sort both before
comparing (`flop_key` in `src/solution.rs`).

## Regenerate a tier

CPU-saturating for hours — **always** wrap in the scavenger script
([docs/shared-machine-data-gen.md](../../../docs/shared-machine-data-gen.md)):

```sh
scripts/idle-run.sh cargo run -p solve-gen --release -- gen --manifest manifests/texture-25.toml
```

Resumable: it skips any (flop × config) whose output files already exist, so
re-running after an interruption is safe and cheap. Variants from the script
header (`scripts/idle-run.sh --help`):

```sh
# survive an SSH disconnect:
setsid nohup scripts/idle-run.sh <command> >run.log 2>&1 < /dev/null &
# also cap RAM on a systemd box:
systemd-run --user --scope -p MemoryMax=12G scripts/idle-run.sh <command>
```

## Verify a run

- File math: 3 files per (formation × flop). `texture-25.toml` = 25 flops × 2
  formations → 150 files: `ls data/solutions/*.json | wc -l`.
- Load every snapshot end to end (a bad file fails loudly):

```console
$ cargo run -q -- report | head -3
flop     texture   node    combos   bet%  ev(bb)  mix
3h8hAh   monotone  IP         537    32%   +3.45  Check 68% · Bet 2.0bb 1% · Bet 4.5bb 31%
3h8hAh   monotone  OOP v33    471    12%   +2.50  Fold 29% · Call 60% · Raise to 4.9bb 12%
```

## Git hygiene (hard rules)

- `git status --porcelain data/` MUST print nothing after a run —
  `.gitignore` blocks new solution JSON by design.
- The tracked set is exactly starter-8: `git ls-files data/solutions | wc -l`
  → **24** (8 flops × 3 nodes, hash `d728de43`).
- **NEVER `git add -f` under `data/solutions/`** unless the owner explicitly
  says the curated committed set is growing.
- The solver's own cache (`~/.cache/poker-trainer/solves/<flop>-<hash8>.bin`)
  is a separate, AGPL-side detail — the trainer never reads it; deleting it
  only costs re-solve time.

## Add a new formation (do ALL steps)

1. Create `data/ranges/<id>/{oop,ip}.txt` — copy an existing formation's
   files as templates; each is one plain solver range string
   (`"22+,A2s+,KTo+,…"`). Hand-curate; never bulk-copy a commercial product's
   charts (design 02).
2. Add a `Formation` entry to the `FORMATIONS` const in `src/solution.rs`
   (~line 163), mirroring an existing one: `id` (must match the ranges dir
   name), `label`, `oop_seat`/`ip_seat`, `pot_bb`, `stack_bb`.
3. If it belongs to a tier, add a `[[runs]]` entry to a manifest:

   ```toml
   [[runs]]
   formation = "<id>"
   flops = "texture-25"    # a [flopsets] name
   ```

4. Regenerate with the tier recipe above.
5. Confirm 3 new files per flop appeared: `ls -t data/solutions | head`.
6. `cargo test` (formation/spot tests live in `src/solution.rs`), then the
   full done checklist. (`drill preflop` no longer reads these files — it
   uses the solved `data/preflop/` charts below.)

## Add a new breadth tier

Copy `manifests/texture-25.toml` as the template: `[flopsets]` are named
card-string lists, plus the generated keyword `"all-iso-flops"` (the 1,755
suit-isomorphic flops). `broad-95` is the next tier the docs name (~95 flops;
no manifest exists yet). New tiers' outputs stay untracked — that is the git
policy, not an accident.

## The preflop chart library (design 07)

Separate from the postflop snapshots: `crates/preflop-gen` (permissive MCCFR,
no solver link) solves the rulesets in `manifests/preflop/*.toml` into
`data/preflop/<id>/`:

```sh
scripts/idle-run.sh cargo run -p preflop-gen --release -- gen
```

Resumable: a ruleset whose `header.json` `config_hash` matches its manifest
is skipped. ~15 min per 6-max ruleset (48M hands); one-off custom configs go
through `preflop-gen solve --ruleset my.toml` and land gitignored. The exact
HU equity table (`data/preflop/equity-hu-169.json`) is committed and only
regenerates via `preflop-gen equity` (a few minutes) if deliberately changed.

Git policy — the **inverse** of solutions: `header.json` + `starter.jsonl`
for the four shipped rulesets ARE committed (the web browser on Pages and a
fresh-clone `drill preflop` read them); `charts.jsonl` (full export) is
gitignored. Never hand-edit; commit a regen only when the manifest or the
generator deliberately changed — the diff is then the point. Never commit a
custom ruleset directory.

Verify a preflop regen:
- `cargo test -p preflop-gen` — includes `shipped_charts_have_sane_shapes`
  (monotone RFI, BB defense, ICM ladder direction) against the committed
  starters.
- `printf '1\nq\n' | cargo run -- drill preflop --ruleset poker-chase-40` —
  one scored spot end-to-end (run-app skill has the recipes).
- `git status data/preflop` shows only deliberate starter/header diffs.
