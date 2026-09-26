"""Train the turn-root value net: (board, pot, both ranges) -> per-hand
counterfactual values in pot units for both players. ~9M-param MLP; loss is
reach-weighted masked MSE (design doc 09, phase b)."""

import argparse
import time

import numpy as np
import torch
from torch import nn

from corpus import Corpus, IN_DIM, OUT_DIM, batches, features


def build_model(in_dim=IN_DIM):
    return nn.Sequential(
        nn.Linear(in_dim, 1024),
        nn.GELU(),
        nn.Linear(1024, 1024),
        nn.GELU(),
        nn.Linear(1024, 1024),
        nn.GELU(),
        nn.Linear(1024, OUT_DIM),
    )


def weighted_mse(pred, y, w):
    return (w * (pred - y) ** 2).sum() / w.sum().clamp_min(1e-9)


def run_split(model, corpus, split, batch, rng, device, opt=None, rake=None):
    total, denom = 0.0, 0.0
    for arr in batches(corpus, split, batch, rng):
        x, y, w = (torch.from_numpy(a).to(device) for a in features(arr, rake))
        pred = model(x)
        loss = weighted_mse(pred, y, w)
        if opt is not None:
            opt.zero_grad()
            loss.backward()
            opt.step()
        ws = float(w.sum())
        total += float(loss.detach()) * ws
        denom += ws
    return total / denom if denom > 0 else float("nan")  # nan: split is empty


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--data", required=True, help="corpus dir (corpus.json + shards)")
    ap.add_argument("--epochs", type=int, default=20)
    ap.add_argument("--batch", type=int, default=8192)
    ap.add_argument("--lr", type=float, default=3e-4)
    ap.add_argument("--out", default="value-net.pt")
    ap.add_argument("--seed", type=int, default=0)
    ap.add_argument("--init", default=None, help="fine-tune from this checkpoint")
    ap.add_argument("--formations", default=None, help="comma-separated corpus dirs to train on (default all)")
    args = ap.parse_args()

    device = "cuda" if torch.cuda.is_available() else "cpu"
    torch.manual_seed(args.seed)
    rng = np.random.default_rng(args.seed)
    corpus = Corpus(args.data, only=args.formations.split(",") if args.formations else None)
    state = torch.load(args.init, map_location=device)["model"] if args.init else None
    in_dim = state["0.weight"].shape[1] if state else IN_DIM
    rake = corpus.rake if in_dim == IN_DIM else None  # v1 checkpoints predate the rake inputs
    model = build_model(in_dim).to(device)
    if state:
        model.load_state_dict(state)
    opt = torch.optim.AdamW(model.parameters(), lr=args.lr)
    sched = torch.optim.lr_scheduler.CosineAnnealingLR(opt, T_max=args.epochs)
    print(f"device={device} params={sum(p.numel() for p in model.parameters()):,}")

    best = float("inf")
    for epoch in range(1, args.epochs + 1):
        t0 = time.time()
        model.train()
        train_loss = run_split(model, corpus, "train", args.batch, rng, device, opt, rake)
        model.eval()
        with torch.no_grad():
            val_loss = run_split(model, corpus, "val", args.batch, rng, device, rake=rake)
        sched.step()
        mark = ""
        if not val_loss >= best:  # also saves when the val split is empty (nan)
            best = val_loss
            torch.save({"model": model.state_dict(), "in_dim": in_dim}, args.out)
            mark = " *"
        print(
            f"epoch {epoch:3}: train {train_loss:.6f}  val {val_loss:.6f}"
            f"  ({time.time() - t0:.0f}s){mark}"
        )
    print(f"best val {best:.6f} -> {args.out}")


if __name__ == "__main__":
    main()
