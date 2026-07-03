# Design 03 — Study mode v2: tree browser, views, reports (P7, P8)

**Status: shipped** (all five milestones — P7 tree browsing/lenses/runouts and
P8 `report`/`equity`/blocker column). This doc is design rationale; current
state lives in [00-overview](00-overview.md).

**Depends on:** 01 (tree walking, runouts, weights/equity), 02 (report
breadth). **Unlocks:** the nodelocking UI (06) reuses the node editor surface.

## Problem

`table` shows one node's strategy colors and cycles a fixed 3-node set. GTO
Wizard's study mode owes its value to *navigation* (any line, any runout) and
*alternate lenses* (range mass, EV, equity) over the same grid.

## Tree browsing (P7)

`table --board …` (and eventually curated spots too) runs on a `TreeSession`
instead of a snapshot list.

- **Action bar**: the current node's actions, numbered; pressing the number
  descends. `u` = back up, `r` = root. The line so far renders as a breadcrumb
  (`BB check · BTN bet 2bb · BB call · [Turn]`), which doubles as the spot
  header the drills already print.
- **Chance nodes**: a 13×4 card picker (ranks × suits, dead cards dimmed).
  Pick a card → `deal`.
- Snapshot fallback: without a session (curated files, `--offline`), `[`/`]`
  node cycling keeps working exactly as today. Same render code — a
  `TreeNode` and a `SolvedSpot` both reduce to the existing `Cell` grid.

## Views (P7)

One grid, four lenses, toggled by key. All are per-cell reductions the
protocol payload already carries (weights, equity, evs, freqs):

| key | lens | cell encoding |
|---|---|---|
| `s` | strategy (today) | action-mix color split |
| `w` | range | brightness = combo mass reaching this node (weights) |
| `e` | EV | color scale on best-action EV, bb |
| `y` | equity | color scale on equity vs. villain's reaching range |

Side panel per focused hand adds: reach weight, equity, per-suit-combo rows
(the data is per-combo already; the panel just stops averaging).

Filters: `f` cycles made-hand buckets (reuse `eval::classify_hand`) — cells
dim unless ≥ half their combos match. Cheap, and it's the study companion to
the range drill's buckets.

## Runouts view (P7)

At a chance node, `o` renders the 13×4 card grid colored by the *next* node's
aggregate strategy per runout (e.g. OOP check/bet split after each turn card),
from the `runouts` op. Enter descends into that card. This is GTO Wizard's
runouts tab, minus animation.

## Aggregate flop report (P8)

CLI, not TUI, first — this is the Pio-scripting-shaped feature and our
comparative strength:

```sh
poker-trainer report --manifest broad-95 --formation srp-btn-bb \
    [--node ip] [--sort bet-freq] [--csv out.csv]
```

One row per (flop, node) over the snapshot library: texture class (from
`texture.rs`), check%, bet% per size, avg pot share, hero EV. Terminal table
+ `--csv`. Group summaries by texture bucket at the bottom (mean ± spread).
Needs 02's breadth to say anything; runs entirely off snapshots (no solver).

## Equity & blocker tools (P8)

- `poker-trainer equity --oop <range> --ip <range> --board <cards>`:
  range-vs-range equity, plus a terminal histogram of the equity
  distribution per range (`eval` already does the Monte Carlo; add the
  range-vs-range wrapper).
- Blocker column in the table side panel: % of villain's *continue* combos
  the focused hand blocks, computed from the villain node's freqs + card
  removal. Snapshot pairs already contain both sides for curated spots;
  sessions get it from two `node` calls.

## Milestones

1. (P7) `table` on `TreeSession`: action bar, breadcrumb, chance picker,
   back/root; snapshot fallback intact.
2. (P7) The four lenses + bucket filter + per-suit side panel.
3. (P7) Runouts view.
4. (P8) `report` command with CSV; texture rollups.
5. (P8) `equity` command + blocker panel column.

## Out of scope

- Charts/plots beyond terminal tables and color grids (no plotting deps; CSV
  is the escape hatch to real plotting tools).
- Saved "study sessions"/bookmarks — a breadcrumb is reproducible by hand;
  add persistence only if navigation depth makes it painful in practice.
