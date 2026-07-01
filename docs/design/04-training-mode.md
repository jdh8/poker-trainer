# Design 04 — Training v2: full hands, persistent leaks, preflop (P5)

**Depends on:** 01 (tree sessions) for full-hand drills; 02 for preflop
charts and formation variety. **Unlocks:** 05 reuses the stats aggregator.

## Problem

Drills score isolated flop nodes. Real leaks live across streets (barreling,
turn probes, river bluff-catching), and today a session's stats evaporate on
exit. GTO Wizard's Practice = full-hand play + a persistent leak profile;
its Play mode is the same thing without scoring. One feature covers both.

## Full-hand drill (`drill hand`)

Runs on a `TreeSession`:

1. Sample a spot (formation × flop per filters), start a session (cache hit =
   instant; else the solve-time warning applies).
2. Deal hero a hand weighted by its range mass at the root.
3. Walk the tree: **villain nodes** sample an action from the equilibrium mix
   for villain's (hidden, range-sampled) hand; **chance nodes** deal uniform
   from unblocked cards; **hero nodes** prompt exactly like `drill gto` does
   today and record EV loss.
4. Terminal node: reveal villain, show the hand replay — one line per hero
   decision: street, line, GTO mix, your pick, EV loss — and running totals.

Villain sampling per-hand (not aggregate) keeps runouts honest: a villain
that check-raises does so with the right part of its range, so hero's later
streets face realistic ranges.

Filters: `--formation`, `--texture wet|dry|paired|…` (reuse
`texture::classify`), `--position oop|ip`. Street-scoped drills ("turn spots
only") are *not* built: a full hand already visits them, and mid-tree entry
needs belief-state bookkeeping that adds no training value over "play the
whole hand".

## Persistent stats

Append-only JSONL — no database. Path: `$XDG_DATA_HOME/poker-trainer/history.jsonl`
(fallback `~/.local/share/…`; tiny stdlib helper, no new dep).

```jsonc
{"v":1, "ts":1720000000, "drill":"hand", "formation":"srp-btn-bb",
 "flop":"td9d6h", "texture":"two-tone", "street":"turn", "hand":"AsKh",
 "bucket":"TopPair", "line":["Check","Bet 2.0bb"], "chosen":"Call",
 "best":"Raise to 6.0bb", "ev_loss":0.31, "gto_freq":0.12}
```

Every scored decision in every drill (`gto`, `range`, `hand`, `preflop`)
appends one record. Write failures warn and never block a drill.

`poker-trainer stats [--by formation|street|texture|bucket] [--last N]`:
count, avg EV loss, accuracy (chosen action ≥5% GTO frequency — the existing
convention), blunder rate, per group; worst groups first; a trend line
comparing the last 200 decisions to the prior 200. Severity bands, used
everywhere: **blunder** > 0.30 bb, **error** 0.05–0.30 bb, **ok** < 0.05 bb.

The aggregator is a pure function over `Vec<StatRecord>` — 05 feeds it
analyze records to get the same report for real hands.

## Preflop drill (`drill preflop`)

No solver involved: charts from `data/ranges/` (02) are the answer key. Deal
a hand + a formation's decision (open? defend vs. 3-bet?), answer
raise/call/fold, score against the chart (frequency match; EV is not
available from charts — accuracy only, recorded with `ev_loss: null`).
Small, fast, and it closes the "GTO Wizard trains preflop too" gap for the
formations the library actually covers.

## Milestones

1. `drill hand` on live-solved spots (`--board`), replay summary, JSONL
   recording for all drills.
2. `stats` command with groupings, severity bands, trend.
3. Filters + curated-library sampling once 02 lands breadth.
4. `drill preflop` off the chart files.

## Out of scope

- Exploitative villain personas ("calling station mode") — nodelocking (06)
  is the principled version; a persona is just a saved locked profile, so it
  falls out of P10 rather than being hand-coded here.
- Timed/scored gamification (GTO points, streaks, leaderboards). EV loss and
  accuracy are the signal; the rest is retention design for a SaaS.
- Multi-session spaced repetition of failed spots — worth revisiting once
  `history.jsonl` shows what repeat-failure data actually looks like.
