"""Evaluate the value net on held-out flops: reach-weighted MAE of cfv/pot,
per formation and side, against the equity baseline (cfv/pot := equity for
the OOP side — the sanity bar the net must clearly beat)."""

import argparse

import numpy as np
import torch

from corpus import N_COMBOS, Corpus, features
from train import build_model


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--data", required=True)
    ap.add_argument("--ckpt", default="value-net.pt")
    ap.add_argument("--batch", type=int, default=8192)
    args = ap.parse_args()

    device = "cuda" if torch.cuda.is_available() else "cpu"
    model = build_model().to(device)
    model.load_state_dict(torch.load(args.ckpt, map_location=device)["model"])
    model.eval()
    corpus = Corpus(args.data)

    print(f"{'formation':<12} {'side':<4} {'net MAE %pot':>12} {'net MAE bb':>11} {'equity-baseline %pot':>21}")
    for f, mm in corpus.shards("val"):
        eq = corpus.val_equity(f)
        err = {"oop": [0.0, 0.0, 0.0], "ip": [0.0, 0.0, 0.0]}  # Σw|e|, Σw|e|·pot, Σw
        base = [0.0, 0.0]  # Σw|equity − cfv|, Σw
        for start in range(0, len(mm), args.batch):
            arr = np.asarray(mm[start : start + args.batch])
            x, y, w = features(arr)
            with torch.no_grad():
                pred = model(torch.from_numpy(x).to(device)).cpu().numpy()
            ae = np.abs(pred - y)
            pot = arr["pot_bb"][:, None]
            for side, sl in (("oop", slice(0, N_COMBOS)), ("ip", slice(N_COMBOS, None))):
                err[side][0] += float((w[:, sl] * ae[:, sl]).sum())
                err[side][1] += float((w[:, sl] * ae[:, sl] * pot).sum())
                err[side][2] += float(w[:, sl].sum())
            e = np.abs(eq[start : start + args.batch].astype(np.float32) - y[:, :N_COMBOS])
            base[0] += float((w[:, :N_COMBOS] * e).sum())
            base[1] += float(w[:, :N_COMBOS].sum())
        for side in ("oop", "ip"):
            s, sp, sw = err[side]
            b = base[0] / max(base[1], 1e-9) * 100 if side == "oop" else float("nan")
            print(
                f"{f['dir']:<12} {side:<4} {s / max(sw, 1e-9) * 100:>11.2f}%"
                f" {sp / max(sw, 1e-9):>11.3f} {b:>20.2f}%"
            )


if __name__ == "__main__":
    main()
