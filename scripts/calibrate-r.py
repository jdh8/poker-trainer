#!/usr/bin/env python3
"""Calibrate preflop-gen's realization factor against the postflop table store.

For every 2-player see-a-flop line with a (near-)complete flop set under
data/tables, average each hand's solver-exact postflop EV over all flops
(iso-multiplicity weighted) and divide by what preflop-gen would value the
same terminal at with R = 1: (pot - rake) x class equity vs the villain's
range. The ratio is R(class, position). Sources are blended by each class's
share of the hero range in that source.

Writes crates/preflop-gen/src/r_table.rs. Run with ionice/nice: it streams
~50 MB per flop off the bulk HDD.

    scripts/calibrate-r.py [--min-files N] [--limit N] [DIR ...]
"""
import argparse
import collections
import glob
import itertools
import json
import os
import sys

import numpy as np

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
RANKS = "AKQJT98765432"
SUITS = "cdhs"


def class_of(h):
    a, b = RANKS.index(h[0]), RANKS.index(h[2])
    hi, lo = min(a, b), max(a, b)
    return hi * 13 + lo if h[1] == h[3] else lo * 13 + hi


def class_name(i):
    r, c = divmod(i, 13)
    if r == c:
        return RANKS[r] * 2
    return RANKS[r] + RANKS[c] + "s" if r < c else RANKS[c] + RANKS[r] + "o"


def card_bit(c):
    return 1 << (RANKS.index(c[0]) * 4 + SUITS.index(c[1]))


def iso(board):
    """Canonical key of a flop and how many distinct real flops share it."""
    seen = set()
    for p in itertools.permutations(SUITS):
        m = dict(zip(SUITS, p))
        seen.add(tuple(sorted(c[0] + m[c[1]] for c in board)))
    return min(seen), len(seen)


def shallow_records(path):
    """Root (OOP) record and the IP records one action deep."""
    root, ip = None, {}
    with open(path, "rb") as f:
        for line in f:
            i = line.find(b'"line":[')
            if i < 0:
                continue
            j = line.find(b"]", i + 8)
            if line.find(b",", i + 8, j) >= 0:
                continue  # two or more actions deep
            r = json.loads(line)
            if not r["line"]:
                root = r
            elif r["player"] == "ip":
                ip[r["line"][0]] = r
    return root, ip


def flop_values(root, ip_nodes):
    """Per-hand (hand, weight, ev) for OOP and IP at the flop root."""
    hands_o = root["hands"]
    w_o = np.array(root["weights"], dtype=np.float64)
    f_o = np.array(root["freqs"], dtype=np.float64)  # [action, hand]
    e_o = np.array(root["evs"], dtype=np.float64)
    ev_o = (f_o * e_o).sum(0)

    actions = root["actions"]
    nodes = [ip_nodes.get(a) for a in actions]
    present = [n for n in nodes if n is not None]
    hands_i = present[0]["hands"]
    for n in present:
        assert n["hands"] == hands_i, "IP hand lists differ across OOP actions"
    w_i = np.array(present[0]["weights"], dtype=np.float64)

    m_o = np.array([card_bit(h[:2]) | card_bit(h[2:]) for h in hands_o])
    m_i = np.array([card_bit(h[:2]) | card_bit(h[2:]) for h in hands_i])
    compat = ((m_i[:, None] & m_o[None, :]) == 0).astype(np.float64)  # [ip, oop]
    denom = compat @ w_o
    ev_i = np.zeros(len(hands_i))
    for a, node in enumerate(nodes):
        p_a = (compat @ (w_o * f_o[a])) / denom  # P(oop action a | ip hand)
        if node is None:
            continue  # unreached OOP action: zero weight anyway
        f_a = np.array(node["freqs"], dtype=np.float64)
        e_a = np.array(node["evs"], dtype=np.float64)
        ev_i += p_a * (f_a * e_a).sum(0)
    return (hands_o, w_o, ev_o), (hands_i, w_i, ev_i)


def pick_hash(d, min_files):
    best = None
    for h in glob.glob(os.path.join(d, "header-*.json")):
        hh = os.path.basename(h)[7:-5]
        n = len(glob.glob(os.path.join(d, f"*-{hh}.jsonl")))
        if n >= min_files and (best is None or n > best[1]):
            best = (hh, n)
    return best


def calibrate_source(d, hh, eq169, limit):
    header = json.load(open(os.path.join(d, f"header-{hh}.json")))
    cfg = header["config"]
    pot = cfg["pot_bb"]
    paid = pot - min(pot * cfg["rake_rate"], cfg["rake_cap_bb"])
    acc_ev = np.zeros((2, 169))
    acc_w = np.zeros((2, 169))
    seen = set()
    files = sorted(glob.glob(os.path.join(d, f"*-{hh}.jsonl")))[:limit]
    zs = []
    for path in files:
        board = os.path.basename(path).split("-")[0]
        board = [board[i : i + 2] for i in range(0, 6, 2)]
        key, mult = iso(board)
        if key in seen:
            continue
        seen.add(key)
        root, ip_nodes = shallow_records(path)
        if root is None or not ip_nodes:
            print(f"  skip {path}: no shallow records", file=sys.stderr)
            continue
        assert abs(root["pot_bb"] - pot) < 1e-6
        sides = flop_values(root, ip_nodes)
        for side, (hands, w, ev) in enumerate(sides):
            cls = np.array([class_of(h) for h in hands])
            np.add.at(acc_ev[side], cls, mult * w * ev)
            np.add.at(acc_w[side], cls, mult * w)
        zs.append(sum((w * ev).sum() / w.sum() for _, w, ev in sides) / pot)
    frac = acc_w / acc_w.sum(1, keepdims=True)  # class share of each range
    with np.errstate(invalid="ignore", divide="ignore"):
        v = acc_ev / acc_w  # flop-averaged EV per class, bb
        # preflop equity of hero class vs villain range (class-level, like preflop-gen)
        eq_pre = np.stack([eq169 @ frac[1], eq169 @ frac[0]])
        r = v / (paid * eq_pre)
    print(
        f"{os.path.basename(d)} {hh}: {len(seen)} flops, pot {pot}, paid {paid:.2f}, "
        f"zero-sum check (oop+ip)/pot = {np.mean(zs):.3f}"
    )
    return r, frac


def grid(vals):
    out = []
    for r in range(13):
        row = []
        for c in range(13):
            x = vals[r * 13 + c]
            row.append("  .  " if np.isnan(x) else f"{x:5.2f}")
        out.append(" ".join(row))
    return "\n".join(out)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("dirs", nargs="*")
    ap.add_argument("--min-files", type=int, default=1469)
    ap.add_argument("--limit", type=int, default=None, help="flops per source (smoke test)")
    ap.add_argument("--out", default=os.path.join(ROOT, "crates/preflop-gen/src/r_table.rs"))
    args = ap.parse_args()

    eq169 = np.array(json.load(open(os.path.join(ROOT, "data/preflop/equity-hu-169.json")))["equity"])
    eq169 = eq169.reshape(169, 169)

    dirs = args.dirs or sorted(glob.glob(os.path.join(ROOT, "data/tables/*/")))
    sources = []
    for d in dirs:
        d = d.rstrip("/")
        pick = pick_hash(d, args.min_files)
        if pick is None:
            print(f"{os.path.basename(d)}: no hash with >= {args.min_files} flops, skipped")
            continue
        r, frac = calibrate_source(d, pick[0], eq169, args.limit)
        sources.append((os.path.basename(d), pick, r, frac))

    num = np.zeros((2, 169))
    den = np.zeros((2, 169))
    for _, _, r, frac in sources:
        ok = ~np.isnan(r)
        num[ok] += (frac * r)[ok]
        den[ok] += frac[ok]
    with np.errstate(invalid="ignore", divide="ignore"):
        table = num / den
    for side, name in enumerate(["OOP", "IP"]):
        print(f"\nR {name} (rows/cols A..2; upper = suited, lower = offsuit):\n{grid(table[side])}")
    json.dump(
        {
            "sources": [
                {"dir": n, "hash": p[0], "flops": p[1], "r": r.tolist(), "frac": f.tolist()}
                for n, p, r, f in sources
            ],
            "table": table.tolist(),
        },
        open(os.path.join(os.environ.get("R_DUMP_DIR", "/tmp"), "r_calibration.json"), "w"),
    )

    src_lines = "\n".join(f"//!   {n} ({p[0]}, {p[1]} flops)" for n, p, _, _ in sources)

    def rust_row(vals):
        return ",\n".join(
            "        " + ", ".join("f32::NAN" if np.isnan(x) else f"{x:.4f}" for x in vals[i : i + 13])
            for i in range(0, 169, 13)
        )

    with open(args.out, "w") as f:
        f.write(
            f"""//! Realization factors calibrated against the postflop table store.
//!
//! Generated by `scripts/calibrate-r.py` — do not hand-edit. Sources:
{src_lines}
//!
//! `R_TABLE[ip as usize][class]`: flop-averaged solver EV of the class over
//! `(pot - rake) x class equity vs the villain range`, blended across sources
//! by the class's share of the hero range. `NaN` = the class never sees a
//! flop in any source; `r_factor` falls back to the hand-shape heuristic.

/// `[oop, ip]` heads-up realization factor by 169-class (design 07).
#[rustfmt::skip]
#[allow(clippy::approx_constant)] // data; a value can land on a named constant
pub const R_TABLE: [[f32; 169]; 2] = [
    [
{rust_row(table[0])},
    ],
    [
{rust_row(table[1])},
    ],
];
"""
        )
    print(f"\nwrote {args.out}")


if __name__ == "__main__":
    main()
