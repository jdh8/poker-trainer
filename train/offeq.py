"""Off-equilibrium turn-root labels (design doc 09, phase c).

    check: how far the net is from exact turn-rooted solves at the reaches
           dls.py actually visited — the out-of-distribution gap, bucketed by
           CFR iteration (reach-weighted MAE in % pot, like eval.py).
    pack:  labels -> a corpus shard in the export-value-corpus record layout,
           registered in the corpus.json of --data as its own formation.

    uv run offeq.py check labels.jsonl --ckpt value-net-200.pt
    uv run offeq.py pack labels.jsonl --data /srv/var/poker/valuenet-v2 --name srp-btn-bb.offeq
"""

import argparse
import json
from pathlib import Path

import numpy as np
import torch

from corpus import IN_DIM, N_COMBOS, RECORD, features_raw
from dls import card_id
from train import build_model


def chunks(path, n=2048):
    """Stream the labels file in chunks of parsed rows (solver errors skipped):
    a full round is 300k+ rows × 5k floats, far too big to hold as objects."""
    rows = []
    for line in open(path):
        if line.strip():
            j = json.loads(line)
            if "oop_cfv" in j:
                rows.append(j)
                if len(rows) == n:
                    yield rows
                    rows = []
    if rows:
        yield rows


def arrays(rows):
    oop = np.array([r["oop"] for r in rows], np.float32)
    ip = np.array([r["ip"] for r in rows], np.float32)
    board = np.array([[card_id(c) for c in r["board"]] for r in rows])
    pot = np.array([r["pot_bb"] for r in rows], np.float32)
    rake = np.array([[r["rake_rate"], r["rake_cap_bb"]] for r in rows], np.float32)
    y = np.array([r["oop_cfv"] + r["ip_cfv"] for r in rows], np.float32)
    return oop, ip, board, pot, rake, y


EDGES = [(-1, -1), (1, 40), (41, 100), (101, 200), (201, 10**9)]


def check(args):
    device = "cuda" if torch.cuda.is_available() else "cpu"
    state = torch.load(args.ckpt, map_location=device)["model"]
    in_dim = state["0.weight"].shape[1]
    net = build_model(in_dim).to(device)
    net.load_state_dict(state)
    net.eval()
    # per bucket: n, Σw|e| oop, Σw oop, Σw|e| ip, Σw ip
    acc = {e: np.zeros(5) for e in EDGES}
    n = secs = secs_max = expl = 0.0
    for rows in chunks(args.labels):
        oop, ip, board, pot, rake, y = arrays(rows)
        x = features_raw(oop, ip, board, pot, rake if in_dim == IN_DIM else None)
        with torch.no_grad():
            pred = net(torch.from_numpy(x).to(device)).cpu().numpy()
        ae = np.abs(pred - y)
        w = np.concatenate([oop, ip], 1)
        it = np.array([r["iter"] for r in rows])
        for lo, hi in EDGES:
            m = (it >= lo) & (it <= hi)
            if m.any():
                acc[(lo, hi)] += [m.sum(), (w[m, :N_COMBOS] * ae[m, :N_COMBOS]).sum(), w[m, :N_COMBOS].sum(),
                                  (w[m, N_COMBOS:] * ae[m, N_COMBOS:]).sum(), w[m, N_COMBOS:].sum()]
        s = np.array([r["secs"] for r in rows])
        n += len(rows)
        secs += s.sum()
        secs_max = max(secs_max, s.max())
        expl += sum(r["exploitability_pot"] for r in rows)
    print(f"{int(n)} labels, solve {secs / n:.1f} s each (max {secs_max:.1f}), exploitability {100 * expl / n:.2f}% pot")
    print(f"{'iterations':>12} {'n':>5} {'OOP MAE %pot':>13} {'IP MAE %pot':>12}")
    for lo, hi in EDGES:
        c, eo, wo, ei, wi = acc[(lo, hi)]
        if c:
            label = "final avg" if lo == -1 else f"{lo}-{hi if hi < 10**9 else ''}"
            print(f"{label:>12} {int(c):5d} {100 * eo / wo:13.2f} {100 * ei / wi:12.2f}")


def fnv1a64(s):
    h = 0xCBF29CE484222325
    for b in s.encode():
        h = ((h ^ b) * 0x100000001B3) & 0xFFFFFFFFFFFFFFFF
    return h


def pack(args):
    data = Path(args.data)
    meta = json.loads((data / "corpus.json").read_text())
    fid = max(f["id"] for f in meta["formations"]) + 1
    flop_ids = {f: i for i, f in enumerate(meta["flops"])}
    paths = {s: data / f"{args.name}.{s}.bin" for s in ("train", "val")}
    out = {s: open(p, "wb") for s, p in paths.items()}
    eqp = data / f"{args.name}.val-equity.bin"
    eq = open(eqp, "wb")  # eval.py's baseline column; zeros, not meaningful here
    count = {"train": 0, "val": 0}
    flops = {"train": set(), "val": set()}
    pot_sum = stack_sum = 0.0
    r0 = None
    for rows in chunks(args.labels):
        r0 = r0 or rows[0]
        oop, ip, board, pot, rake, y = arrays(rows)
        rec = np.zeros(len(rows), RECORD)
        rec["formation_id"] = fid
        rec["flags"] = 1
        rec["board"] = board
        rec["pot_bb"] = pot
        rec["reach"] = 1.0
        rec["oop_reach"], rec["ip_reach"] = oop, ip
        rec["oop_cfv"], rec["ip_cfv"] = y[:, :N_COMBOS], y[:, N_COMBOS:]
        for r in rows:
            flop_ids.setdefault(r["flop"].lower(), len(flop_ids))
        rec["flop_id"] = [flop_ids[r["flop"].lower()] for r in rows]
        is_val = np.array([fnv1a64(r["flop"].lower()) % 10 == 0 for r in rows])
        for split, m in (("train", ~is_val), ("val", is_val)):
            rec[m].tofile(out[split])
            count[split] += int(m.sum())
            flops[split] |= {r["flop"] for r, k in zip(rows, m) if k}
        np.zeros((int(is_val.sum()), N_COMBOS), "<f2").tofile(eq)
        pot_sum += pot.sum()
        stack_sum += sum(r["stack_bb"] for r in rows)
    for f in out.values():
        f.close()
    eq.close()
    total = count["train"] + count["val"]
    meta["flops"] = [f for f, _ in sorted(flop_ids.items(), key=lambda kv: kv[1])]
    meta["formations"].append({
        "id": fid, "name": f"{r0['formation']} off-equilibrium turn roots", "dir": args.name,
        "config_hashes": [], "pot_bb": pot_sum / total, "stack_bb": stack_sum / total,
        "rake_rate": r0["rake_rate"], "rake_cap_bb": r0["rake_cap_bb"],
        "files": len(flops["train"] | flops["val"]), "skipped_files": 0, "roots": total,
        "train": {"file": paths["train"].name, "records": count["train"], "flops": len(flops["train"])},
        "val": {"file": paths["val"].name, "records": count["val"], "flops": len(flops["val"])},
        "val_equity_file": eqp.name,
    })
    (data / "corpus.json").write_text(json.dumps(meta, indent=1))
    print(f"packed {total} labels as formation {fid} ({count['train']} train / {count['val']} val)")


def main():
    ap = argparse.ArgumentParser()
    sub = ap.add_subparsers(dest="cmd", required=True)
    c = sub.add_parser("check")
    c.add_argument("labels")
    c.add_argument("--ckpt", default="value-net-200.pt")
    c.set_defaults(fn=check)
    p = sub.add_parser("pack")
    p.add_argument("labels")
    p.add_argument("--data", required=True)
    p.add_argument("--name", required=True)
    p.set_defaults(fn=pack)
    args = ap.parse_args()
    args.fn(args)


if __name__ == "__main__":
    main()
