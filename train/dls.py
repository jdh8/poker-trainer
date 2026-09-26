"""Design-09 phase (c) prototype: a depth-limited flop solve whose turn-root
leaves are the value net, validated against the stored full solve of the
same flop.

Tree shape comes from `solve-gen tree` (the solver's own flop-street action
tree), fold leaves are exact, the all-in leaf is the exact showdown matrix
from `flop-equity`, and every other street-closing node asks the net for both
sides' turn-root values under the current reaches. Vector DCFR over all 1326
combos per side with card-removal-correct counterfactual sums. Reports the
reach-weighted distance of the root strategy/EV (and of every stored flop
decision node's strategy) from the solver's.

    uv run dls.py --formation srp-btn-bb --flop 2c7d7h --ckpt value-net-200.pt
"""

import argparse
import json
import subprocess
import time
from pathlib import Path

import numpy as np
import torch

from corpus import IN_DIM, N_COMBOS, features_raw
from train import build_model

RANKS = "23456789TJQKA"
SUITS = "cdhs"
# Combo i = (hi, lo) with hi > lo at hi*(hi-1)/2 + lo — the corpus order.
COMBOS = np.array([(hi, lo) for hi in range(52) for lo in range(hi)], dtype=np.int64)
# BLOCK[c] = combos holding card c.
BLOCK = np.zeros((52, N_COMBOS), bool)
BLOCK[COMBOS[:, 0], np.arange(N_COMBOS)] = True
BLOCK[COMBOS[:, 1], np.arange(N_COMBOS)] = True
REPO = Path(__file__).resolve().parent.parent


def card_id(s):
    return RANKS.index(s[0].upper()) * 4 + SUITS.index(s[1].lower())


def combo_of(hand):
    a, b = card_id(hand[:2]), card_id(hand[2:])
    hi, lo = max(a, b), min(a, b)
    return hi * (hi - 1) // 2 + lo


def expand_class(cls):
    """`AA`, `AKs`, `A5s+`, `22+`, `22-55`, `K9o-KJo` -> [(hi, lo, kind)]."""
    if "-" in cls:
        a, b = cls.split("-")
        ra, rb = (RANKS.index(a[0]), RANKS.index(a[1])), (RANKS.index(b[0]), RANKS.index(b[1]))
        if ra[0] == ra[1]:
            lo, hi = sorted((ra[0], rb[0]))
            return [(r, r, "p") for r in range(lo, hi + 1)]
        lo, hi = sorted((ra[1], rb[1]))
        return [(ra[0], k, a[2:]) for k in range(lo, hi + 1)]
    plus = cls.endswith("+")
    cls = cls.rstrip("+")
    h, k, kind = RANKS.index(cls[0]), RANKS.index(cls[1]), cls[2:]
    if h == k:
        return [(r, r, "p") for r in range(h, 13 if plus else h + 1)]
    if h < k:
        h, k = k, h
    return [(h, kk, kind) for kk in range(k, h if plus else k + 1)]


def class_combos(hi, lo, kind):
    out = []
    for s1 in range(4):
        for s2 in range(4):
            if hi == lo:
                if s2 >= s1:
                    continue
            elif (kind == "s" and s1 != s2) or (kind == "o" and s1 == s2):
                continue
            a, b = hi * 4 + s1, lo * 4 + s2
            out.append(max(a, b) * (max(a, b) - 1) // 2 + min(a, b))
    return out


def parse_range(s):
    """Solver range syntax -> 1326 weights (later tokens override)."""
    w = np.zeros(N_COMBOS, np.float64)
    for tok in s.split(","):
        tok = tok.strip()
        if not tok:
            continue
        cls, _, wt = tok.partition(":")
        for hi, lo, kind in expand_class(cls):
            w[class_combos(hi, lo, kind)] = float(wt) if wt else 1.0
    return w


def compat_mass(r):
    """M[h] = mass of `r` on combos sharing no card with h."""
    cs = np.bincount(COMBOS[:, 0], r, 52) + np.bincount(COMBOS[:, 1], r, 52)
    return r.sum() - cs[COMBOS[:, 0]] - cs[COMBOS[:, 1]] + r


class Node:
    def __init__(self, j, parent, idx, line):
        self.parent, self.idx, self.line = parent, idx, line
        self.player = {"oop": 0, "ip": 1}.get(j["player"], -1)
        self.kind = j["player"]  # oop | ip | terminal | chance
        self.pot, self.matched = j["pot"], j["matched_pot"]
        self.folder, self.allin = j.get("folder"), j.get("allin", False)
        self.actions = j.get("actions", [])
        self.children = []


def flatten(j):
    """DFS order (parents before children)."""
    nodes = []

    def walk(j, parent, idx, line):
        n = Node(j, parent, idx, line)
        nodes.append(n)
        if parent is not None:
            parent.children.append(n)
        for i, (a, c) in enumerate(zip(n.actions, j.get("children", []))):
            walk(c, n, i, line + [a])

    walk(j, None, None, [])
    return nodes


def regret_match(r):
    pos = np.maximum(r, 0.0)
    s = pos.sum(0, keepdims=True)
    return np.where(s > 0, pos / np.maximum(s, 1e-30), 1.0 / len(r))


class DLS:
    def __init__(self, tree_json, net, in_dim, equity, device, alpha=1.5, beta=0.0, gamma=2.0, stored=None):
        self.cfg = tree_json["config"]
        self.flop = [card_id(tree_json["flop"][i : i + 2]) for i in (0, 2, 4)]
        self.nodes = flatten(tree_json["tree"])
        self.dec = [n for n in self.nodes if n.player >= 0]
        self.deal = [n for n in self.nodes if n.kind == "chance" and not n.allin]
        self.turns = [c for c in range(52) if c not in self.flop]
        self.net, self.in_dim, self.E, self.device = net, in_dim, equity, device
        self.alpha, self.beta, self.gamma = alpha, beta, gamma
        live = ~BLOCK[self.flop].any(0)
        self.r0 = np.stack([parse_range(self.cfg["oop_range"]), parse_range(self.cfg["ip_range"])]) * live
        for n in self.dec:
            n.regret = np.zeros((len(n.children), N_COMBOS))
            n.cum = np.zeros((len(n.children), N_COMBOS))
            n.sigma = np.full((len(n.children), N_COMBOS), 1.0 / len(n.children))
        self.rake = lambda pot: min(pot * self.cfg["rake_rate"], self.cfg["rake_cap_bb"])
        # Diagnostic: freeze every deal leaf's net input at the solver's
        # equilibrium reaches (walked from the stored freqs) so leaf values
        # are constants with only the net's in-distribution error in them.
        self.frozen = None
        if stored is not None:
            self.frozen = {}
            for n in self.deal:
                r = self.stored_reach(stored, n.line)
                if r is not None:
                    self.frozen[id(n)] = r
            print(f"  frozen leaves: {len(self.frozen)}/{len(self.deal)} deal nodes have stored prefixes")

    def stored_reach(self, stored, line):
        r = self.r0.copy()
        for i in range(len(line)):
            s = stored.get(tuple(line[:i]))
            if s is None:
                return None
            p = {"oop": 0, "ip": 1}[s["player"]]
            idx = np.array([combo_of(h) for h in s["hands"]])
            f = np.asarray(s["freqs"], np.float64)[s["actions"].index(line[i])]
            step = np.zeros(N_COMBOS)
            step[idx] = f
            r[p] *= step
        return r

    # -- one traversal under the nodes' current `sigma` ---------------------
    def traverse(self):
        root = self.nodes[0]
        root.reach = self.r0.copy()
        for n in self.nodes[1:]:
            p = n.parent
            n.reach = p.reach.copy()
            n.reach[p.player] *= p.sigma[n.idx]
        self.eval_deal_leaves()
        for n in reversed(self.nodes):
            if n.kind == "terminal":
                f, w = n.folder, 1 - n.folder
                P, rk = n.matched, self.rake(n.matched)
                n.u = np.zeros((2, N_COMBOS))
                n.u[w] = (P / 2 - rk) * compat_mass(n.reach[f])
                n.u[f] = -(P / 2) * compat_mass(n.reach[w])
            elif n.kind == "chance" and n.allin:
                P, rk = n.matched, self.rake(n.matched)
                n.u = np.stack(
                    [(P - rk) * (self.E @ n.reach[1 - p]) - (P / 2) * compat_mass(n.reach[1 - p]) for p in (0, 1)]
                )
            elif n.kind == "chance":
                pass  # n.u set by eval_deal_leaves
            else:
                p = n.player
                n.u = np.zeros((2, N_COMBOS))
                for a, c in enumerate(n.children):
                    n.u[p] += n.sigma[a] * c.u[p]
                    n.u[1 - p] += c.u[1 - p]

    def eval_deal_leaves(self):
        """Batch every (deal node, turn card) through the net; leaf value =
        mean over the 45 turn cards a hand pair leaves, in the solver's
        half-pot convention, counterfactually weighted by the opponent's
        turn-masked compatible mass."""
        if not self.deal:
            return
        rows_o, rows_i, boards, pots = [], [], [], []
        for n in self.deal:
            rr = n.reach if self.frozen is None else self.frozen.get(id(n), n.reach)
            for c in self.turns:
                rows_o.append(rr[0] * ~BLOCK[c])
                rows_i.append(rr[1] * ~BLOCK[c])
                boards.append(self.flop + [c])
                pots.append(n.matched)
        oop, ip = np.asarray(rows_o, np.float32), np.asarray(rows_i, np.float32)
        rake = None
        if self.in_dim == IN_DIM:
            rake = np.tile([self.cfg["rake_rate"], self.cfg["rake_cap_bb"]], (len(pots), 1)).astype(np.float32)
        x = features_raw(oop, ip, np.asarray(boards), np.asarray(pots, np.float32), rake)
        with torch.no_grad():
            ev = self.net(torch.from_numpy(x).to(self.device)).float().cpu().numpy()
        k = 0
        for n in self.deal:
            n.u = np.zeros((2, N_COMBOS))
            for c in self.turns:
                for p, sl in ((0, slice(0, N_COMBOS)), (1, slice(N_COMBOS, None))):
                    opp = (ip if p == 0 else oop)[k].astype(np.float64)
                    n.u[p] += n.matched * (ev[k, sl] - 0.5) * compat_mass(opp) * ~BLOCK[c]
                k += 1
            n.u /= 45.0

    def iterate(self, t):
        for n in self.dec:
            n.sigma = regret_match(n.regret)
        self.traverse()
        dp = t**self.alpha / (t**self.alpha + 1)
        dn = t**self.beta / (t**self.beta + 1)
        ds = (t / (t + 1)) ** self.gamma
        for n in self.dec:
            p = n.player
            inst = np.stack([c.u[p] for c in n.children]) - n.u[p]
            n.regret = np.where(n.regret > 0, n.regret * dp, n.regret * dn) + inst
            n.cum = n.cum * ds + n.reach[p] * n.sigma

    def average(self):
        """Switch every node to its DCFR average strategy and re-traverse so
        `u` reflects it (for EV reporting)."""
        for n in self.dec:
            s = n.cum.sum(0, keepdims=True)
            n.sigma = np.where(s > 0, n.cum / np.maximum(s, 1e-30), n.sigma)
        self.traverse()


def net_check(dls, stored):
    """The net at the solver's own reaches vs the stored OOP turn-root values:
    reach-weighted MAE in % pot, the number the eval harness reports."""
    num = den = 0.0
    for n in dls.deal:
        rr = dls.frozen.get(id(n)) if dls.frozen else None
        if rr is None:
            continue
        for c in dls.turns:
            s = stored.get(tuple(n.line + [f"deal {RANKS[c // 4]}{SUITS[c % 4]}"]))
            if s is None:
                continue
            idx = np.array([combo_of(h) for h in s["hands"]])
            ev_st = (np.asarray(s["freqs"]) * np.asarray(s["evs"])).sum(0) / s["pot_bb"]
            w = np.asarray(s["weights"])
            oop, ip = rr[0] * ~BLOCK[c], rr[1] * ~BLOCK[c]
            rake = None
            if dls.in_dim == IN_DIM:
                rake = np.array([[dls.cfg["rake_rate"], dls.cfg["rake_cap_bb"]]], np.float32)
            x = features_raw(oop[None].astype(np.float32), ip[None].astype(np.float32),
                             np.array([dls.flop + [c]]), np.array([s["pot_bb"]], np.float32), rake)
            with torch.no_grad():
                ev = dls.net(torch.from_numpy(x).to(dls.device)).cpu().numpy()[0, :N_COMBOS]
            num += (w * np.abs(ev[idx] - ev_st)).sum()
            den += w.sum()
    return 100 * num / max(den, 1e-9)


def stored_nodes(path):
    out = {}
    with open(path) as f:
        for line in f:
            if line.strip():
                j = json.loads(line)
                out[tuple(j["line"])] = j
    return out


def compare(dls, stored):
    """Reach-weighted L1 strategy distance (0..1) per stored flop decision
    node, and the root EV MAE in % of the starting pot."""
    rows = []
    for n in dls.dec:
        s = stored.get(tuple(n.line))
        if s is None:
            continue
        assert s["actions"] == n.actions, (n.line, s["actions"], n.actions)
        idx = np.array([combo_of(h) for h in s["hands"]])
        w = np.asarray(s["weights"], np.float64)
        f = np.asarray(s["freqs"], np.float64)  # (A, H)
        sig = n.sigma[:, idx]
        l1 = (w * np.abs(sig - f).sum(0)).sum() / (2 * w.sum())
        ev_mae = None
        if not n.line:  # root: matched pot, unambiguous EV convention
            p = n.player
            M = compat_mass(n.reach[1 - p])[idx]
            ev = np.stack([c.u[p][idx] for c in n.children]) / np.maximum(M, 1e-30) + n.matched / 2
            ev_dls = (sig * ev).sum(0)
            ev_st = (f * np.asarray(s["evs"], np.float64)).sum(0)
            ev_mae = (w * np.abs(ev_dls - ev_st)).sum() / w.sum() / n.matched * 100
        rows.append((n.line, s["reach"], l1, ev_mae))
    return rows


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--tables", default="/srv/var/poker/tables")
    ap.add_argument("--formation", default="srp-btn-bb", help="curated id, or --from for grounded")
    ap.add_argument("--from", dest="from_", default=None)
    ap.add_argument("--flop", nargs="+", required=True)
    ap.add_argument("--ckpt", default="value-net-200.pt")
    ap.add_argument("--iters", type=int, default=400)
    ap.add_argument("--log-every", type=int, default=100)
    ap.add_argument("--cache", default=str(REPO / "target" / "dls-cache"))
    ap.add_argument("--leaf-reach", choices=["live", "stored"], default="live",
                    help="stored = diagnostic: net inputs frozen at the solver's equilibrium reaches")
    ap.add_argument("--hands", nargs="*", default=[], help="print root freqs/EVs for these hands")
    args = ap.parse_args()

    device = "cuda" if torch.cuda.is_available() else "cpu"
    state = torch.load(args.ckpt, map_location=device)["model"]
    in_dim = state["0.weight"].shape[1]
    net = build_model(in_dim).to(device)
    net.load_state_dict(state)
    net.eval()
    cache = Path(args.cache)
    cache.mkdir(parents=True, exist_ok=True)
    spot = ["--from", args.from_] if args.from_ else ["--formation", args.formation]
    fdir = (args.from_ or args.formation).replace(":", "_")

    summary = []
    for flop in args.flop:
        tree_path = cache / f"tree-{fdir}-{flop}.json"
        if not tree_path.exists():
            tree_path.write_bytes(
                subprocess.check_output([REPO / "target/release/solve-gen", "tree", "--flop", flop, *spot])
            )
        tj = json.loads(tree_path.read_text())
        equity = None
        if any(n.get("allin") for n in flatten_json(tj["tree"])):
            eq_path = cache / f"eq-{flop}.f32"
            if not eq_path.exists():
                subprocess.check_call([REPO / "target/release/flop-equity", flop, "--out", eq_path])
            equity = np.fromfile(eq_path, dtype="<f4").reshape(N_COMBOS, N_COMBOS).astype(np.float64)
        stored = stored_nodes(Path(args.tables) / fdir / f"{flop.lower()}-{tj['hash']}.jsonl")

        dls = DLS(tj, net, in_dim, equity, device, stored=stored if args.leaf_reach == "stored" else None)
        if dls.frozen is not None:
            print(f"  net at equilibrium reaches vs stored turn roots: OOP MAE {net_check(dls, stored):.2f}% pot")
        t0 = time.time()
        for t in range(1, args.iters + 1):
            dls.iterate(t)
            if t % args.log_every == 0 or t == args.iters:
                dls.average()
                root = compare(dls, stored)[0]
                print(f"{flop} it {t:4d}  root L1 {root[2]:.4f}  root EV MAE {root[3]:.2f}% pot  ({time.time() - t0:.0f}s)")
                for n in dls.dec:
                    n.sigma = regret_match(n.regret)
        dls.average()
        rows = compare(dls, stored)
        root = rows[0]
        if args.hands:
            rn, s = dls.nodes[0], stored[()]
            M = compat_mass(rn.reach[1])
            print(f"  {'hand':6} " + "  ".join(f"{a[:12]:>26}" for a in rn.actions) + "   (freq dls/st, ev dls/st bb)")
            for h in args.hands:
                i, k = combo_of(h), s["hands"].index(h) if h in s["hands"] else None
                if k is None:
                    print(f"  {h:6} not in stored hands"); continue
                cells = []
                for a, c in enumerate(rn.children):
                    ev = c.u[0][i] / max(M[i], 1e-30) + rn.matched / 2
                    cells.append(f"{rn.sigma[a][i]:.2f}/{s['freqs'][a][k]:.2f} {ev:6.2f}/{s['evs'][a][k]:6.2f}")
                print(f"  {h:6} " + "  ".join(f"{c:>26}" for c in cells))
        deeper = [(r[1], r[2]) for r in rows[1:]]
        wl1 = sum(r * l for r, l in deeper) / max(sum(r for r, _ in deeper), 1e-9) if deeper else float("nan")
        print(f"{flop}: root L1 {root[2]:.4f}, root EV MAE {root[3]:.2f}% pot, "
              f"{len(deeper)} deeper stored nodes reach-weighted L1 {wl1:.4f}")
        summary.append((flop, root[2], root[3], wl1))
    if len(summary) > 1:
        a = np.array([s[1:] for s in summary])
        print(f"MEAN over {len(summary)} flops: root L1 {a[:,0].mean():.4f}, root EV MAE {a[:,1].mean():.2f}% pot, deeper L1 {a[:,2].mean():.4f}")


def flatten_json(j):
    yield j
    for c in j.get("children", []):
        yield from flatten_json(c)


if __name__ == "__main__":
    main()
