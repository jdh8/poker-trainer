# 10 — End-to-end flop lookup on GitHub Pages

Status: **planned** (2026-09-28), nothing shipped. Four session-sized
phases below; each leaves `main` green and deployable on its own.

Goal: on the public site, walk a preflop chart to a heads-up flop, type the
three flopped cards, and see the solved flop grids for that exact line and
board — no server, no live solve. Locally this already works
(`scripts/serve-web.sh` + the "Any flop" input, [08](08-instant-flops.md));
this doc is about making it fit Pages and wiring the two browsers together.

## What exists

- `ground.rs` names a grounded spot `<ruleset>:<line>`; its table dir is
  `formation_dir()` = `cash-hu55_r2.5-c`. Line tokens are `preflop.rs`'s:
  `f c x r<size> ai`.
- 27 grounded line dirs are solved locally, 1,755 canonical flops each
  (cash-hu55 ×7, cash89 ×10, mtt89 ×10) plus the 5 curated formations.
  cash-hu34 / mtt-hu34 are still NFS-only.
- `export-tables-web` (src/main.rs) reshapes flop decision nodes into
  per-flop JSONL; `web/app.js` `tbBoardEntered` serves any typed flop via
  `canonical_flop` + suit relabeling.
- The preflop browser (`pfRender`) walks `starter.jsonl` by token path. When
  the next node is missing it says "Line not stored" — true for pruned
  lines, wrong for lines that simply closed to a flop.

## Blockers

1. **Size.** Pages caps a published site at 1 GB. Measured over 10 stored
   srp-btn-bb flops (10 flop nodes, ~519 combos each):

   | format | per flop | per line (1,755) | lines in ~900 MB |
   |---|---|---|---|
   | JSONL (today) | 383 KB | 670 MB | 1 |
   | JSONL + gzip | 57 KB | 100 MB | 9 |
   | u8 freq + i16 EV + gzip | 21 KB | 37 MB | 24 |
   | u8 freq only + gzip | 7 KB | 12 MB | 70+ |

   Quantized freq+EV is the pick: all of cash-hu55 plus the 17 heaviest
   cash89/mtt89 lines by arrival mass. gzip-JSONL needs no new format but
   leaves room for cash-hu55 only.
2. **Git bloat.** A 900 MB tier can't be committed; every regen would add
   it to history. Pages deploys from a fresh checkout, so the data has to
   arrive from somewhere else during the workflow.
3. **Token bug.** `pfTok` (web/app.js) maps `Check` to `rCheck`; the
   generator and manifests use `x`. Invisible so far because the HU starter
   tier ends at the flop; fatal for the `cash-hu55_c-x` join.

## Phases

Each phase is one session. Order matters: 1 defines the file format the
rest consume; 4 is independent and can go first if a session is short.

### Phase 1 — compact export format (Rust)

- `run_export_tables_web` writes `<stem>-<hash>.bin` per flop: one JSON
  header line (`[{line, hero_oop, pot_bb, board, actions, label,
  villain_action}]`, one entry per node, same order as today) then raw
  bytes per node: a 1,326-bit combo mask over the canonical combo order
  (rank-major, suits `c d h s`), then `freqs` as u8 (×255) and `evs` as i16
  centi-bb, action-major.
- gzip via the `gzip` CLI in the publish script (phase 2), not a new crate.
- `index.json` drops `display` (JS titlecases the stem) — 24 × 1,755 stems
  is ~1 MB either way, Pages gzips on the wire.
- Regenerate the committed `data/tables-web` in the new format (30 MB →
  ~2 MB) so the browser has one code path. Round-trip test in `main.rs`:
  decode the bytes back and compare to `PostflopTable` within quantization.
- DoD green; the site is briefly broken until phase 3 lands, so **land
  phases 1 and 3 in the same commit** if they split across sessions (keep 1
  on a branch).

### Phase 2 — data transport (script + workflow)

- `scripts/publish-tables-web.sh`: export the shipped line dirs (a list in
  the script, ordered by arrival mass, cut at ~900 MB total), gzip, tar,
  `gh release upload tables-web --clobber`. One release tag, reused.
- `pages.yml`: after copying the committed `data/tables-web`, `gh release
  download tables-web` and untar over `_site/tables`. Curated tier from git,
  grounded tier from the release, one `index.json` (the export writes it
  from everything it exported, so the release tarball's index wins — it
  includes the curated formations too).
- Ceiling: 1 GB. Upgrade path when hu34 tiers arrive: fetch straight from
  release assets with `Range` (CORS-safelisted, S3 backend honours it) and
  an offsets index — removes the cap entirely, but needs a curl check that
  the github.com redirect carries `Access-Control-Allow-Origin`.

### Phase 3 — browser decode + preflop hand-off (JS)

- `tbLoad`: fetch `.bin.gz`, pipe through native
  `DecompressionStream('gzip')`, split header line from bytes, expand mask
  → hand names from a fixed 1,326-combo table, rebuild the shape
  `tbReshape` feeds `renderGrid`. ~30 lines.
- Fix `pfTok`: `Check` → `x`. Add a native test in `web/src/lib.rs` (or a
  tiny JS assert) that the token set matches `preflop.rs::token`.
- `pfRender`: when `pfNodes[path]` is missing and
  `tbIndex[`${id}_${path}`]` exists, render "Flop: [___] Show tables"
  instead of "Line not stored". On enter set `tb-formation`, `tb-board`,
  call `tbBoardEntered()`, scroll `#tables` into view. The table index is
  the authority on which lines are flop lines — no `ground.rs` legality
  re-implemented in JS. Missing table → say so and point at the equity
  explorer.
- `TB_FORMATION_LABELS` fallback for grounded dirs: `cash-hu55_r2.5-c` →
  "cash-hu55 · SB raises 2.5, BB calls" via the preflop verbs already in
  `pfVerb`.

### Phase 4 — verify and ship

- `scripts/serve-web.sh --export` (new format), then: cash-hu55 root → SB
  raise 2.5 → BB call → type `Td9d6h` → the OOP root grid must match
  `table --from cash-hu55:r2.5-c --board Td9d6h` (run-app skill recipe) on
  the same node within quantization.
- Run `publish-tables-web.sh`, push, confirm the Pages deploy size and that
  the public site serves a grounded flop cold.
- Update the parity matrix in [00](00-overview.md) and the "Local web"
  paragraph in [08](08-instant-flops.md).

## Not doing

- Per-formation index split — only if the single index parses slowly.
- Turn/river nodes on the site — flop decision nodes only, as today.
- A second Pages repo to double the budget — the Range-on-release path is
  the better ceiling breaker and needs no extra repo.
