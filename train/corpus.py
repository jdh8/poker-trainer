"""Reader for the export-value-corpus shards (record layout mirrored from
src/bin/export-value-corpus.rs; corpus.json is the source of truth)."""

import json
from pathlib import Path

import numpy as np

N_COMBOS = 52 * 51 // 2

RECORD = np.dtype(
    [
        ("flop_id", "<u2"),
        ("formation_id", "u1"),
        ("flags", "u1"),
        ("board", "u1", 4),
        ("pot_bb", "<f4"),
        ("reach", "<f4"),
        ("oop_reach", "<f2", N_COMBOS),
        ("ip_reach", "<f2", N_COMBOS),
        ("oop_cfv", "<f2", N_COMBOS),
        ("ip_cfv", "<f2", N_COMBOS),
    ]
)

IN_DIM = 2 * N_COMBOS + 2 + 52 + 1  # ranges, their masses, board, pot
OUT_DIM = 2 * N_COMBOS


class Corpus:
    def __init__(self, root):
        self.root = Path(root)
        self.meta = json.loads((self.root / "corpus.json").read_text())
        assert self.meta["record_bytes"] == RECORD.itemsize, "layout drift"

    def shards(self, split):
        """Yield (formation meta, records memmap) for non-empty shards."""
        for f in self.meta["formations"]:
            n = f[split]["records"]
            if n:
                path = self.root / f[split]["file"]
                yield f, np.memmap(path, dtype=RECORD, mode="r", shape=(n,))

    def val_equity(self, formation):
        n = formation["val"]["records"]
        path = self.root / formation["val_equity_file"]
        return np.memmap(path, dtype="<f2", mode="r", shape=(n, N_COMBOS))


def batches(corpus, split, batch, rng, chunk=32768):
    """HDD-friendly batching: shards in random order, each read sequentially
    in big chunks, rows shuffled in RAM within the chunk."""
    shards = list(corpus.shards(split))
    rng.shuffle(shards)
    for _, mm in shards:
        for start in range(0, len(mm), chunk):
            arr = np.asarray(mm[start : start + chunk])
            idx = rng.permutation(len(arr))
            for b in range(0, len(arr), batch):
                yield arr[idx[b : b + batch]]


def features(batch):
    """Model input (float32): L1-normalized ranges + raw masses + board
    multi-hot + pot/10. Returns (x, y, w) numpy arrays; w is the per-slot
    loss weight (reach; IP side gated by flags bit0)."""
    oop = batch["oop_reach"].astype(np.float32)
    ip = batch["ip_reach"].astype(np.float32)
    so = oop.sum(1, keepdims=True)
    si = ip.sum(1, keepdims=True)
    board = np.zeros((len(batch), 52), np.float32)
    board[np.arange(len(batch))[:, None], batch["board"].astype(int)] = 1.0
    x = np.concatenate(
        [
            oop / np.maximum(so, 1e-9),
            ip / np.maximum(si, 1e-9),
            so,
            si,
            board,
            batch["pot_bb"][:, None] / 10.0,
        ],
        axis=1,
    )
    y = np.concatenate(
        [batch["oop_cfv"].astype(np.float32), batch["ip_cfv"].astype(np.float32)], axis=1
    )
    ip_on = (batch["flags"][:, None] & 1).astype(np.float32)
    w = np.concatenate([oop, ip * ip_on], axis=1)
    return x, y, w
