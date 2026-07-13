# data/tables-web

Committed, deploy-ready **web export** of the reach-pruned postflop tables
(`data/tables/`, gitignored). One JSONL per flop holding only the
**flop-decision nodes** (no turn/river), reshaped into the 13×13-grid shape
`web/app.js` reads. Powers the "Postflop tables" section of the site.

- **Generated — never hand-edit.** Regenerate with:
  `cargo run --release -- export-tables-web`
  (pure post-processing of local `data/tables/`; links no solver, runs no solve).
- Committed on purpose (unlike `data/tables/`): the Pages deploy runs on a
  fresh checkout, so only git-tracked data ships. Regenerate and commit only
  when `data/tables/` or the export shape deliberately changed.
- Layout: `<formation>/<flop>-<hash8>.jsonl` (one node per line) + `index.json`
  (the formation → flops catalog the browser fetches). Each node hoists
  `actions` and stores per-combo `freqs`/`evs`; the browser re-nests them.
