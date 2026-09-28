//! External-sampling MCCFR over the preflop game (design 07).
//!
//! Seeded and batch-parallel: identical seed + budget ⇒ bit-identical
//! output for any thread count (each hand owns an RNG stream; a batch's
//! hands read the table as of the batch start and merge in hand order). Each
//! hand deals real cards from a real deck (exact card removal
//! for free), maps every seat's holding to its 169-class, then runs one
//! traversal per seat: the traverser explores all of its actions, everyone
//! else samples from their current regret-matched strategy. Regret-matching+
//! (negative regrets clamped) with quadratically-weighted averaging.
// ponytail: plain external sampling — the ceiling is sample variance, not
// iteration count. Measured 2026-09-28 by exact HU exploitability (10bb
// push/fold at 2M hands, mtt-hu21 check-down at 20M): discounting regrets
// DCFR-style (increments × k^α) is 30–70% *worse* for α = 0.5–1.5, and
// merging K independent seeds' averages is ~2× worse than one seed; only
// quadratic averaging (DCFR's γ = 2) paid, ~5%. Threads (`threads`) buy
// wall-clock for one solve, not throughput. Upgrade: a vectorized CFR+
// backend for the 2-player subgame, behind the same NodeData layout.

use crate::equity::{Deal, EquityCache};
use crate::game::{Ruleset, State};
use crate::icm::TerminalValuer;
use poker_trainer::preflop::{class_index, CLASSES};
use rand::rngs::SmallRng;
use rand::{RngExt, SeedableRng};
use rs_poker::core::{Card, Deck};
use std::collections::HashMap;

/// Per-infoset accumulators, row-major `[class][action]`.
///
/// Per-node sums stay small (hundreds of visits per class even on huge
/// budgets, spread across ~10^5 states), so f32 accumulation is safe.
pub struct NodeData {
    /// Clamped (RM+) cumulative regrets.
    pub regret: Vec<f32>,
    /// Quadratically-weighted average-strategy numerator.
    pub strat_sum: Vec<f32>,
    /// Quadratically-weighted per-action counterfactual value numerator — exports
    /// as the per-action EV (value vs the evolving strategy profile; design
    /// 07 documents the caveat).
    pub cfv_sum: Vec<f32>,
    /// Per-class denominator for `cfv_sum`.
    pub cfv_weight: Vec<f32>,
    /// Actions at this node (the row stride).
    pub actions: usize,
}

impl NodeData {
    fn new(actions: usize) -> Self {
        NodeData {
            regret: vec![0.0; CLASSES * actions],
            strat_sum: vec![0.0; CLASSES * actions],
            cfv_sum: vec![0.0; CLASSES * actions],
            cfv_weight: vec![0.0; CLASSES],
            actions,
        }
    }

    /// Regret-matching+ strategy for one class (uniform when all regrets 0).
    pub fn strategy(&self, class: usize, out: &mut Vec<f32>) {
        let row = &self.regret[class * self.actions..(class + 1) * self.actions];
        let total: f32 = row.iter().sum();
        out.clear();
        if total > 0.0 {
            out.extend(row.iter().map(|r| r / total));
        } else {
            out.extend(std::iter::repeat_n(1.0 / self.actions as f32, self.actions));
        }
    }

    /// Normalized average strategy for one class (uniform if never updated).
    pub fn average(&self, class: usize) -> Vec<f32> {
        let row = &self.strat_sum[class * self.actions..(class + 1) * self.actions];
        let total: f32 = row.iter().sum();
        if total > 0.0 {
            row.iter().map(|s| s / total).collect()
        } else {
            vec![1.0 / self.actions as f32; self.actions]
        }
    }

    /// Average per-action EV for one class, `None` before the first visit.
    pub fn action_ev(&self, class: usize) -> Option<Vec<f32>> {
        let w = self.cfv_weight[class];
        (w > 0.0).then(|| {
            self.cfv_sum[class * self.actions..(class + 1) * self.actions]
                .iter()
                .map(|v| v / w)
                .collect()
        })
    }
}

/// Hands per batch. Every hand in a batch plays against the regrets as they
/// stood at the batch's start; the deltas then merge in hand order. Fixed —
/// not per-thread — so the output never depends on the thread count.
// ponytail: fixed batch, sequential merge — the merge is the Amdahl ceiling;
// shard it by key if many-core scaling ever matters.
const BATCH: u64 = 4096;

/// The solver: lazy infoset table + terminal machinery + batch workers.
pub struct Solver<'a> {
    /// The game being solved.
    pub rs: &'a Ruleset,
    /// Infosets keyed by [`State::key`], allocated at the first merged
    /// traverser or opponent visit.
    pub infosets: HashMap<u64, NodeData>,
    valuer: TerminalValuer,
    equity: EquityCache,
    rng: SmallRng, // exact-BR only (HU terminals never sample)
    hands_dealt: u64,
    avg_warmup: u64,
    workers: Vec<Worker>,
}

/// What a batch's workers share, read-only.
struct Ctx<'s> {
    rs: &'s Ruleset,
    infosets: &'s HashMap<u64, NodeData>,
    valuer: &'s TerminalValuer,
    equity: &'s EquityCache,
    seed: u64,
    avg_warmup: u64,
}

/// One infoset visit to fold into the table at merge time; its payload sits
/// in [`Worker::vals`]: traverser `[w, regret deltas.., w·cfv..]`, opponent
/// `[w·σ..]`.
struct Visit {
    key: u64,
    class: u8,
    actions: u8,
    traverser: bool,
}

/// Per-thread traversal state and delta log.
struct Worker {
    rng: SmallRng,
    deck: Vec<Card>,
    deal: Deal,
    scratch: Vec<Vec<f32>>, // per-depth strategy buffers (avoid re-allocating)
    log: Vec<Visit>,
    vals: Vec<f32>,
}

impl<'a> Solver<'a> {
    /// New single-threaded solver seeded from the ruleset's `[solver]` params.
    pub fn new(rs: &'a Ruleset, equity: EquityCache) -> Self {
        Solver {
            rs,
            infosets: HashMap::new(),
            valuer: TerminalValuer::new(rs),
            equity,
            rng: SmallRng::seed_from_u64(rs.solver.seed),
            hands_dealt: 0,
            avg_warmup: 0,
            workers: Vec::new(),
        }
        .threads(1)
    }

    /// Traverse each batch on `n` threads. Output is bit-identical for any
    /// `n` (per-hand RNG streams, hand-ordered merge).
    pub fn threads(mut self, n: usize) -> Self {
        self.workers = (0..n.max(1))
            .map(|_| Worker {
                rng: SmallRng::seed_from_u64(0),
                deck: Vec::new(),
                deal: Deal::class_level([0; 6]),
                scratch: Vec::new(),
                log: Vec::new(),
                vals: Vec::new(),
            })
            .collect();
        self
    }

    /// Delayed averaging: strategy/EV sums stay zero-weighted for the first
    /// `hands` hands, so the average never carries the early-iteration noise
    /// (uniform-ish opponents stacking off inflates every EV otherwise).
    /// Regrets always update. Call before the first `run`.
    pub fn set_avg_warmup(&mut self, hands: u64) {
        self.avg_warmup = hands;
    }

    /// Hands dealt so far.
    pub fn hands_dealt(&self) -> u64 {
        self.hands_dealt
    }

    /// Switch see-a-flop terminals to check-down equity (the `R ≡ 1` A/B
    /// baseline). Call before the first `run`.
    pub fn check_down(mut self) -> Self {
        self.valuer = TerminalValuer::new(self.rs).check_down();
        self
    }

    /// Deal `hands` more hands, running one traversal per seat per hand.
    pub fn run(&mut self, hands: u64) {
        let end = self.hands_dealt + hands;
        while self.hands_dealt < end {
            let (lo, hi) = (self.hands_dealt, end.min(self.hands_dealt + BATCH));
            let ctx = Ctx {
                rs: self.rs,
                infosets: &self.infosets,
                valuer: &self.valuer,
                equity: &self.equity,
                seed: self.rs.solver.seed,
                avg_warmup: self.avg_warmup,
            };
            let per = (hi - lo).div_ceil(self.workers.len() as u64);
            if let [w] = &mut self.workers[..] {
                w.play(&ctx, lo, hi);
            } else {
                std::thread::scope(|s| {
                    for (i, w) in self.workers.iter_mut().enumerate() {
                        let a = (lo + i as u64 * per).min(hi);
                        let b = (a + per).min(hi);
                        let ctx = &ctx;
                        s.spawn(move || w.play(ctx, a, b));
                    }
                });
            }
            for w in &mut self.workers {
                w.merge_into(&mut self.infosets);
            }
            self.hands_dealt = hi;
        }
    }

    /// Average strategy at a public state for one class, `None` if the state
    /// was never visited.
    pub fn average_at(&self, st: &State, class: usize) -> Option<Vec<f32>> {
        self.infosets.get(&st.key()).map(|n| n.average(class))
    }

    /// Exact best-response exploitability, **heads-up unraked rulesets
    /// only**: how much a maximizing player gains over the average strategy,
    /// averaged over both seats, in utility units (bb heads-up cash; the
    /// game value cancels because HU-no-rake is constant-sum).
    ///
    /// Class pairs are weighted by their disjoint combo counts, so card
    /// removal is exact.
    pub fn exploitability(&mut self) -> f64 {
        assert!(
            self.rs.n() == 2 && self.rs.rake_rate == 0.0,
            "exact BR is implemented for unraked heads-up only"
        );
        // Joint weights: disjoint combo pairs per class pair.
        let combos: Vec<Vec<[Card; 2]>> = (0..CLASSES).map(crate::equity::combos_of).collect();
        let mut weight = vec![0.0f64; CLASSES * CLASSES];
        let mut total = 0.0f64;
        for i in 0..CLASSES {
            for j in 0..CLASSES {
                let w = combos[i]
                    .iter()
                    .flat_map(|a| combos[j].iter().map(move |b| (a, b)))
                    .filter(|(a, b)| !a.iter().any(|c| b.contains(c)))
                    .count() as f64;
                weight[i * CLASSES + j] = w;
                total += w;
            }
        }

        let mut gain = 0.0f64;
        for hero in 0..2 {
            // Villain arrives with weight[i][j] folded into the reach vector
            // per hero class inside the recursion's terminal sums.
            let reach = vec![1.0f64; CLASSES];
            let vals = self.best_response(State::root(self.rs), hero, &reach, &weight);
            gain += vals.iter().sum::<f64>() / total;
        }
        // Constant-sum: u_SB + u_BB = the total ante at every terminal (the
        // dead ante flows back out of the pot), so BR-sum minus the constant
        // measures total distance from equilibrium; halve it for the usual
        // per-player exploitability.
        (gain - f64::from(self.rs.ante_bb)) / 2.0
    }

    /// BR values for `hero`, summed over villain classes: returns per-hero-
    /// class totals of `weight[i][j] × villain_reach[j] × u_hero`.
    fn best_response(
        &mut self,
        st: State,
        hero: usize,
        villain_reach: &[f64],
        weight: &[f64],
    ) -> Vec<f64> {
        let villain = 1 - hero;
        if st.terminal(self.rs).is_some() {
            let mut out = vec![0.0; CLASSES];
            // Class-level deal: HU terminals never sample boards, so only
            // the classes matter — mutate them in place.
            let mut deal = Deal::class_level([0; 6]);
            for (i, o) in out.iter_mut().enumerate() {
                for j in 0..CLASSES {
                    let w = weight[i * CLASSES + j] * villain_reach[j];
                    if w <= 0.0 {
                        continue;
                    }
                    deal.classes[hero] = i as u8;
                    deal.classes[villain] = j as u8;
                    *o += w * self.valuer.value(
                        self.rs,
                        &st,
                        hero,
                        &deal,
                        &self.equity,
                        &mut self.rng,
                    );
                }
            }
            return out;
        }

        let actor = st.to_act().unwrap() as usize;
        let mut acts = Vec::new();
        st.legal(self.rs, &mut acts);
        // Copy the acting player's averages out so the map borrow doesn't
        // span the recursion.
        let node = self.infosets.get(&st.key());
        let uniform = vec![1.0 / acts.len() as f32; acts.len()];
        let avg: Vec<Vec<f32>> = (0..CLASSES)
            .map(|j| node.map_or_else(|| uniform.clone(), |n| n.average(j)))
            .collect();

        if actor == hero {
            // Maximize per hero class across actions.
            let mut best = vec![f64::NEG_INFINITY; CLASSES];
            for a in &acts {
                let vals = self.best_response(st.apply(self.rs, *a), hero, villain_reach, weight);
                for (b, v) in best.iter_mut().zip(vals) {
                    *b = b.max(v);
                }
            }
            best
        } else {
            // Villain mixes their average strategy into the reach.
            let mut out = vec![0.0; CLASSES];
            for (ai, a) in acts.iter().enumerate() {
                let mut reach = vec![0.0f64; CLASSES];
                let mut any = false;
                for j in 0..CLASSES {
                    let p = villain_reach[j] * f64::from(avg[j][ai]);
                    if p > 0.0 {
                        reach[j] = p;
                        any = true;
                    }
                }
                if !any {
                    continue;
                }
                let vals = self.best_response(st.apply(self.rs, *a), hero, &reach, weight);
                for (o, v) in out.iter_mut().zip(vals) {
                    *o += v;
                }
            }
            out
        }
    }
}

impl Worker {
    /// Play hands `lo..hi` (0-based indices) against the frozen table.
    fn play(&mut self, ctx: &Ctx, lo: u64, hi: u64) {
        let n = ctx.rs.n();
        for k in lo..hi {
            // Each hand owns its RNG stream and a fresh deck, so which thread
            // plays it never matters.
            self.rng = SmallRng::seed_from_u64(ctx.seed ^ k.wrapping_mul(0x9E37_79B9_7F4A_7C15));
            self.deck.clear();
            self.deck.extend(Deck::default());
            // Partial Fisher-Yates: the first 2n cards are this hand's deal.
            for i in 0..2 * n {
                let j = self.rng.random_range(i..52);
                self.deck.swap(i, j);
            }
            for s in 0..n {
                let hole = [self.deck[2 * s], self.deck[2 * s + 1]];
                self.deal.holes[s] = hole;
                self.deal.classes[s] = class_index(hole) as u8;
            }
            self.deal.pool.clear();
            self.deal.pool.extend_from_slice(&self.deck[2 * n..]);
            // Quadratic averaging past the warm-up: hand k (1-based) weighs
            // `(k − warmup)²` (zero during warm-up ⇒ sums untouched).
            let w = (k + 1).saturating_sub(ctx.avg_warmup) as f32;
            let w = w * w;
            for t in 0..n {
                self.traverse(ctx, State::root(ctx.rs), t, w, 0);
            }
        }
    }

    /// External-sampling traversal returning the traverser's utility.
    fn traverse(&mut self, ctx: &Ctx, st: State, t: usize, w: f32, depth: usize) -> f64 {
        let Some(actor) = st.to_act() else {
            return ctx
                .valuer
                .value(ctx.rs, &st, t, &self.deal, ctx.equity, &mut self.rng);
        };
        let actor = actor as usize;
        let class = self.deal.classes[actor] as usize;
        let key = st.key();

        let mut acts = Vec::new();
        st.legal(ctx.rs, &mut acts);
        debug_assert!(acts.len() <= 8);
        if self.scratch.len() <= depth {
            self.scratch.push(Vec::new());
        }
        let mut sigma = std::mem::take(&mut self.scratch[depth]);
        match ctx.infosets.get(&key) {
            Some(node) => {
                debug_assert_eq!(node.actions, acts.len());
                node.strategy(class, &mut sigma);
            }
            None => {
                sigma.clear();
                sigma.resize(acts.len(), 1.0 / acts.len() as f32);
            }
        }
        let visit = Visit {
            key,
            class: class as u8,
            actions: acts.len() as u8,
            traverser: actor == t,
        };

        let value = if actor == t {
            // Explore every action; regret-update against the mixture value.
            let mut vals = [0.0f64; 8];
            let mut node_value = 0.0f64;
            for (i, a) in acts.iter().enumerate() {
                vals[i] = self.traverse(ctx, st.apply(ctx.rs, *a), t, w, depth + 1);
                node_value += f64::from(sigma[i]) * vals[i];
            }
            let vals = &vals[..acts.len()];
            self.log.push(visit);
            self.vals.push(w);
            self.vals
                .extend(vals.iter().map(|v| (v - node_value) as f32));
            self.vals.extend(vals.iter().map(|v| w * *v as f32));
            node_value
        } else {
            // Opponent: accumulate the average strategy, sample one action.
            self.log.push(visit);
            self.vals.extend(sigma.iter().map(|s| w * s));
            let mut roll = self.rng.random_range(0.0..1.0f32);
            let mut pick = acts.len() - 1;
            for (i, s) in sigma.iter().enumerate() {
                if roll < *s {
                    pick = i;
                    break;
                }
                roll -= s;
            }
            self.traverse(ctx, st.apply(ctx.rs, acts[pick]), t, w, depth + 1)
        };
        self.scratch[depth] = sigma;
        value
    }

    /// Fold this worker's logged visits into the table, in log order.
    fn merge_into(&mut self, infosets: &mut HashMap<u64, NodeData>) {
        let mut vals = &self.vals[..];
        for v in self.log.drain(..) {
            let n = v.actions as usize;
            let node = infosets.entry(v.key).or_insert_with(|| NodeData::new(n));
            let row = v.class as usize * n;
            if v.traverser {
                node.cfv_weight[v.class as usize] += vals[0];
                for i in 0..n {
                    let r = &mut node.regret[row + i];
                    *r = (*r + vals[1 + i]).max(0.0); // RM+
                    node.cfv_sum[row + i] += vals[1 + n + i];
                }
                vals = &vals[1 + 2 * n..];
            } else {
                for (s, v) in node.strat_sum[row..row + n].iter_mut().zip(vals) {
                    *s += v;
                }
                vals = &vals[n..];
            }
        }
        self.vals.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::Action;
    use poker_trainer::preflop::{class_combos, class_index_of, class_name};

    fn hu_pushfold(stack_bb: f32, seed: u64) -> Ruleset {
        toml::from_str(&format!(
            r#"
            id = "hu-pf"
            label = "HU push/fold"
            seats = ["SB", "BB"]
            stack_bb = {stack_bb}
            sb = 0.5
            bb = 1.0
            open_to_bb = []
            threebet_mult = [3.0]
            squeeze_mult = [3.0]
            fourbet_mult = [2.3]
            jam_from_level = 0
            no_limps = true
            [solver]
            traversals = 1000
            seed = {seed}
            "#
        ))
        .unwrap()
    }

    fn real_cache() -> EquityCache {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../data/preflop/equity-hu-169.json"
        );
        EquityCache::load(path).expect("committed HU equity table")
    }

    /// Combo-weighted frequency of action `ai` across all classes.
    fn overall_freq(solver: &Solver, st: &State, ai: usize) -> f64 {
        let mut num = 0.0;
        for class in 0..CLASSES {
            let avg = solver.average_at(st, class).unwrap();
            num += f64::from(avg[ai]) * f64::from(class_combos(class));
        }
        num / 1326.0
    }

    #[test]
    fn identical_seeds_reproduce_bit_identical_strategies() {
        let rs = hu_pushfold(10.0, 42);
        let table = vec![0.5; CLASSES * CLASSES];
        let mut a = Solver::new(&rs, EquityCache::new(table.clone()));
        let mut b = Solver::new(&rs, EquityCache::new(table));
        a.run(2_000);
        b.run(2_000);
        let root = State::root(&rs);
        for class in [0, 84, 168] {
            assert_eq!(a.average_at(&root, class), b.average_at(&root, class));
        }
    }

    #[test]
    fn thread_count_never_changes_the_output() {
        let rs = hu_pushfold(10.0, 7);
        let table = vec![0.5; CLASSES * CLASSES];
        let mut a = Solver::new(&rs, EquityCache::new(table.clone()));
        let mut b = Solver::new(&rs, EquityCache::new(table)).threads(3);
        a.run(10_000); // > BATCH, with a ragged last batch
        b.run(10_000);
        for (k, n) in &a.infosets {
            let m = &b.infosets[k];
            assert_eq!((&n.regret, &n.strat_sum), (&m.regret, &m.strat_sum));
            assert_eq!((&n.cfv_sum, &n.cfv_weight), (&m.cfv_sum, &m.cfv_weight));
        }
        assert_eq!(a.infosets.len(), b.infosets.len());
    }

    /// 5-second smoke: with real equities, the certainties show up fast.
    #[test]
    fn hu_pushfold_smoke_learns_the_certainties() {
        let rs = hu_pushfold(10.0, 1);
        let mut solver = Solver::new(&rs, real_cache());
        solver.run(30_000);
        let root = State::root(&rs);
        let jam = State::root(&rs).apply(&rs, Action::AllIn);
        let (aa, so) = (
            class_index_of("AA").unwrap(),
            class_index_of("72o").unwrap(),
        );
        // AA always jams and always calls; 72o mostly folds the call.
        assert!(solver.average_at(&root, aa).unwrap()[1] > 0.95, "AA jam");
        assert!(solver.average_at(&jam, aa).unwrap()[1] > 0.95, "AA call");
        assert!(solver.average_at(&jam, so).unwrap()[1] < 0.2, "72o call");
    }

    /// The published-Nash cross-check (design 07 M3). ~1–2 min release.
    #[test]
    #[ignore = "solves 10bb HU push/fold to convergence (~1-2 min release)"]
    fn hu_pushfold_matches_nash() {
        let rs = hu_pushfold(10.0, 1);
        let mut solver = Solver::new(&rs, real_cache());
        solver.run(2_000_000);
        let eps = solver.exploitability();
        assert!(eps < 0.02, "exploitability {eps} bb");

        let root = State::root(&rs);
        let jam = root.apply(&rs, Action::AllIn);
        // HUNE 10bb: pusher ≈ 58% of hands, caller ≈ 37%.
        let push = overall_freq(&solver, &root, 1);
        let call = overall_freq(&solver, &jam, 1);
        assert!((0.52..0.64).contains(&push), "push {push}");
        assert!((0.32..0.44).contains(&call), "call {call}");

        // Hand-level certainties from the published tables.
        for (name, freq_floor) in [("AA", 0.99), ("22", 0.9), ("A2o", 0.9), ("K7s", 0.9)] {
            let f = solver
                .average_at(&root, class_index_of(name).unwrap())
                .unwrap()[1];
            assert!(f > freq_floor, "{name} pushes {f}");
        }
        for name in ["72o", "83o"] {
            let f = solver
                .average_at(&root, class_index_of(name).unwrap())
                .unwrap()[1];
            assert!(f < 0.1, "{name} pushes {f}");
        }
        for name in ["AA", "KK", "QQ", "AKs"] {
            let f = solver
                .average_at(&jam, class_index_of(name).unwrap())
                .unwrap()[1];
            assert!(f > 0.95, "{name} calls {f}");
        }
        let f = solver
            .average_at(&jam, class_index_of("72o").unwrap())
            .unwrap()[1];
        assert!(f < 0.05, "72o calls {f}");

        // Print the ranges for eyeballing under --nocapture.
        let fmt = |st: &State| {
            (0..CLASSES)
                .filter(|&c| solver.average_at(st, c).unwrap()[1] > 0.5)
                .map(class_name)
                .collect::<Vec<_>>()
                .join(",")
        };
        println!("push: {}", fmt(&root));
        println!("call: {}", fmt(&jam));
    }
}
