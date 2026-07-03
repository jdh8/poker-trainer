---
name: run-app
description: >-
  Run, exercise, or verify poker-trainer and solve-gen from a headless
  session. Use whenever asked to run, launch, or demo the app, verify a
  change end-to-end, drive a drill, pipe ops to solve-gen serve, or run the
  solver-spawning ignored tests. Drills block on stdin and `table` needs a
  real TTY — read this before running anything interactive.
---

# Running & verifying poker-trainer headlessly

Every drill reads stdin line by line and exits cleanly on EOF or `q`, so pipe
answers in with `printf` / `yes`. Two rules before anything else:

1. `drill gto/range/preflop/hand` **append scored decisions to the user's real
   drill history** (`$XDG_DATA_HOME/poker-trainer/history.jsonl`, default
   `~/.local/share/…`). For verification runs, always redirect it:
   `env XDG_DATA_HOME=$(mktemp -d)`.
2. A live solve (`--board` on a cache miss, `drill hand`, serve `solve`) is
   CPU-minutes and ~1 GB RAM. Never start one bare on this shared machine —
   wrap in `scripts/idle-run.sh`.

Build everything first: `cargo build --all-targets`.

## Drills, headlessly

`drill pot-odds` — answer `c`/`f` per spot (no solver, records no history):

```console
$ printf 'c\nq\n' | cargo run -q -- drill pot-odds
Spot #1
  Your hand: 5♠ 4♠
  Flop:      T♠ J♣ 5♦
  Pot 10bb. Villain bets 5.0bb.
  call or fold? >   True equity: 22.6%  (needed 25.0%)
  Best play: FOLD (call EV -0.48bb).  You said call -> wrong
…
Session: 0/1 correct (0%).
```

Pass = a dealt spot, a scored answer, a `Session:` line, exit 0.

`drill texture` — two prompts per spot (suit `r`/`t`/`m`, then paired `y`/`n`):

```console
$ printf 'r\nn\nq\n' | cargo run -q -- drill texture
  Flop: 8♠ T♦ A♣
  Suit pattern? r)ainbow t)wo-tone m)onotone >   Paired? y/n >   Texture: rainbow pattern, unpaired, disconnected, high card A.  -> correct
Session: 1/1 correct (100%).
```

`drill preflop` — answer the shown key (`r`/`c`) or `f`; records history:

```console
$ printf 'f\nq\n' | env XDG_DATA_HOME=$(mktemp -d) cargo run -q -- drill preflop
Spot #1 — SRP CO vs BB
  Your hand: 2♥ Q♠
  You're CO, folded to you. Open-raise?
  r)aise or f)old? >   Chart says: Fold.  You said Fold -> correct
Session: 1/1 correct (100%).
```

`drill gto` — answer by action number; loads the `data/solutions/` library
(works on a fresh clone: the starter-8 set is committed); records history:

```console
$ printf '1\nq\n' | env XDG_DATA_HOME=$(mktemp -d) cargo run -q -- drill gto
Spot #1: 3-bet pot BB vs BTN, QhJd9c — you're BB, facing BTN 75% c-bet: defend?
  Your hand: T♦ T♣
    1) Fold
    2) Call
    3) Raise to 33.8bb
  Your action? (number) >
  GTO mix:
    Fold             0.1%   EV +0.00bb
    Call            92.5%   EV +2.28bb   <- best
  You chose Fold -> EV loss 2.28bb (GTO plays it 0.1%).
Session: 1 spots, 0 on a GTO action (0%), avg EV loss 2.276bb.
```

`drill range` — one spot, one action number per bucket (up to 12 buckets);
`yes 1` answers them all; records history:

```console
$ yes 1 | env XDG_DATA_HOME=$(mktemp -d) cargo run -q -- drill range
Range scored: 141 combos in 12 buckets.
  Avg EV loss: 6.51bb/combo  |  Accuracy: 36% of combos on a GTO action.
  bucket       combos  your action     avg loss  GTO plays it
  Value ▲           8  Fold            41.42bb            0%
…
```

`drill hand` — needs `--board` and starts a real tree-session solve
(CPU-minutes on a cache miss). **Do not use it for casual verification** — the
ignored tests below exercise the same path with a tiny solve.

`table` — a ratatui TUI, **requires a real TTY, never run it headless**.
Verify `table.rs` changes with `cargo test table::`; when visual confirmation
is needed, hand the human the command instead:
`cargo run -- table` (curated snapshots) or
`cargo run -- table --board Td9d6h [--line "Check,Bet 2.0bb"] [--locks f.json]`.

## Non-interactive commands

Run these directly; all read-only:

```console
$ cargo run -q -- stats
11 decisions (/home/jdh8/.local/share/poker-trainer/history.jsonl).
  Avg EV loss 0.018bb (ok) | accuracy 91% | blunders 0 (0%)
  bucket          count   avg loss   accuracy   blunders  band
  TopPair             5    0.026bb        80%         0%  ok
…

$ cargo run -q -- report | head -4
flop     texture   node    combos   bet%  ev(bb)  mix
3h8hAh   monotone  IP         537    32%   +3.45  Check 68% · Bet 2.0bb 1% · Bet 4.5bb 31%
…

$ cargo run -q -- equity --oop "QQ+,AKs" --ip "22+,ATs+,KQs" --board Td9d6h
Computing range-vs-range equity (22 × 88 combos)… done.
Board Td9d6h  —  OOP 63.4%  vs  IP 36.6%
…(equity histograms follow)

$ cargo run -q -- analyze tests/fixtures/pokerstars.txt --dry-run
Parsed 11 hand(s) (1 unparseable block(s) skipped).
Matched 2 hand(s) (18%) — 4 hero postflop decision(s):
…
```

`--dry-run` never touches a solver. `analyze` *without* it solves cache
misses up to `--solve-budget` (default 10m) — that's real CPU; wrap it:
`scripts/idle-run.sh cargo run -q -- analyze <files> --solve-budget 2m`.

## Drive `solve-gen serve` by hand

One JSON request per line on stdin, one response per line on stdout (protocol
v2, schema in [design 01](../../../docs/design/01-tree-protocol.md)). Ops:
`solve, node, play, deal, back, root, snapshot, runouts, lock, resolve, quit`.

The `solve` op needs a **full `SpotConfig`** — pull one out of any snapshot
with `jq` instead of typing it. **Check the solve cache first**: the command
is instant only if the matching `.bin` exists; otherwise it starts a real
solve (then wrap the whole pipe in `scripts/idle-run.sh`).

```console
$ ls ~/.cache/poker-trainer/solves/ | grep td9d6h
td9d6h-289b7689.bin
$ { jq -c '{v:2, op:"solve", config:{flop:"td9d6h", config:.config}}' \
      data/solutions/td9d6h-289b7689-ip.json
    echo '{"op":"node"}'; echo '{"op":"quit"}'
  } | cargo run -q -p solve-gen --release -- serve \
    | jq -c 'if .hands then {player, pot_bb, actions, cached, hands:(.hands|length)} else . end'
  loaded cached solve /home/jdh8/.cache/poker-trainer/solves/td9d6h-289b7689.bin
{"player":"oop","pot_bb":18.0,"actions":["Check","Bet 5.9bb","Bet 13.5bb"],"cached":true,"hands":154}
{"player":"oop","pot_bb":18.0,"actions":["Check","Bet 5.9bb","Bet 13.5bb"],"cached":null,"hands":154}
{"ok":true}
```

(~0.8 s wall on the cache hit. Full node payloads are huge — always trim with
`jq` as above. Errors come back as `{"error":"…"}` on stdout; solve progress
goes to stderr.)

## Solver-boundary tests

Three `#[ignore]` tests spawn a tiny real solve through `serve`
(`tree.rs` walk, `trainer.rs` `--line` descend, `analyze.rs` scoring):

```console
$ cargo test -- --ignored
test result: ok. 3 passed; 0 failed; 0 ignored; …
```

~2 s with a warm solve cache; minutes cold (first run also builds solve-gen
in release). Run them whenever touching `tree.rs`, `LiveSolutionProvider`,
`solve-gen serve`, or the protocol. Setting
`POKER_TRAINER_SOLVE_GEN=/path/to/solve-gen` makes the trainer use a prebuilt
binary instead of the in-tree `cargo run -p solve-gen --release` fallback.

## Done checklist

Before calling any change verified:

```sh
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --all-features
cargo test --all-features
git status --porcelain data/   # MUST print nothing
```
