# data/tables-web

Committed, deploy-ready **web export** of the reach-pruned postflop tables
(`data/tables/`, gitignored): the five curated formations × the texture-25
flops, **flop-decision nodes** only (no turn/river). Powers the "Postflop
tables" section of the site.

- **Generated — never hand-edit.** Regenerate with:

  ```sh
  rm -rf data/tables-web/{srp,3bp}-*
  cargo run --release --bin poker-trainer -- export-tables-web \
    --formation srp-btn-bb,srp-co-bb,srp-sb-bb,3bp-bb-btn,3bp-btn-co \
    --flops 4s3s2d,5c5d9h,5h4h2s,6h5d4c,7c5s3d,8d7d3c,8h8c3d,9s8s4d,acjc9s,ah8h3h,ahad8c,ahqh7c,as7d2c,askd5h,jdtd4s,jh9h5c,kdkh6s,kh7c2d,khqs4d,ksts6c,qc8s3h,qhjd9c,td9d6h,th8c6s,tstc7d
  gzip -9f data/tables-web/*/*.bin
  ```

  (pure post-processing of local `data/tables/`; links no solver, runs no solve).
- Committed on purpose (unlike `data/tables/`): the Pages deploy runs on a
  fresh checkout, so only git-tracked data ships. Regenerate and commit only
  when `data/tables/` or the export shape deliberately changed.
- Layout: `<formation>/<flop>-<hash8>.bin.gz` + `index.json` (formation →
  `{flops: {stem: hash8}}` — grounded tiers hash per flop). Each `.bin` is one JSON header line (per node:
  `line, hero_oop, villain_action, pot_bb, board, actions`) then, per
  node in the same order, a 1,326-bit combo mask, u8 freqs (×255) and
  little-endian i16 EVs (centi-bb), action-major — `encode_node` in
  `src/main.rs`, decoded by `tbFetchNodes` in `web/app.js` (design 10).
