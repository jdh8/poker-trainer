//! Tournament equity (Malmuth–Harville ICM) and terminal-node valuation.
//!
//! Utilities are what MCCFR maximizes: net big blinds for chip-EV rulesets,
//! tournament-equity units (the payout vector's units) under ICM. Antes are
//! sunk before the hand — `stack_bb` is the post-ante betting stack and the
//! baseline every utility is measured against — so they cancel out of every
//! action comparison while still inflating the pot that's fought over.

use crate::equity::{sampled_shares, Deal, EquityCache};
use crate::game::{Ruleset, State, Terminal, CB};
use rand::rngs::SmallRng;

/// Malmuth–Harville finishing-distribution equity: each player's expected
/// payout given `stacks`. Exact recursion over the paid places only —
/// P(i finishes next) among the remaining players is stack-proportional.
pub fn malmuth_harville(stacks: &[f64], payouts: &[f64]) -> Vec<f64> {
    let n = stacks.len();
    let paid = payouts
        .iter()
        .rposition(|&p| p > 0.0)
        .map_or(0, |i| i + 1)
        .min(n);
    let mut eq = vec![0.0; n];
    place_rec(stacks, payouts, paid, 0, 0, 1.0, &mut eq);
    eq
}

fn place_rec(
    stacks: &[f64],
    payouts: &[f64],
    paid: usize,
    place: usize,
    taken: u32,
    prob: f64,
    eq: &mut [f64],
) {
    if place >= paid {
        return;
    }
    let remaining: f64 = stacks
        .iter()
        .enumerate()
        .filter(|(i, _)| taken & (1 << i) == 0)
        .map(|(_, s)| s)
        .sum();
    if remaining <= 0.0 {
        return; // everyone left is felted; deeper places pay nothing here
    }
    for i in 0..stacks.len() {
        if taken & (1 << i) != 0 || stacks[i] <= 0.0 {
            continue;
        }
        let p = prob * stacks[i] / remaining;
        eq[i] += p * payouts[place];
        place_rec(stacks, payouts, paid, place + 1, taken | (1 << i), p, eq);
    }
}

/// What a terminal pays the hero, in the ruleset's utility.
pub enum Utility {
    /// Net big blinds (cash).
    ChipEv,
    /// Malmuth–Harville tournament equity in payout units.
    Icm {
        /// Payout by finish, best first.
        payouts: Vec<f64>,
    },
}

/// Equity-realization factor for a see-a-flop terminal: how much of its raw
/// pot share a hand actually banks across the postflop streets.
// ponytail: THE load-bearing approximation of design 07 — a static
// playability × position × multiway heuristic, tuned until the chart-shape
// tests hold; the upgrade path is calibrating against the in-repo
// data/solutions postflop outputs. Known leak: per-hero factors aren't
// jointly normalized, so see-a-flop terminals aren't exactly zero-sum.
pub fn r_factor(class: usize, last_to_act: bool, players: u32) -> f64 {
    let (r, c) = (class / 13, class % 13);
    let (hi, lo) = (r.min(c), r.max(c)); // rank indices, 0 = ace
    let hi_v = (12 - hi) as f64 / 12.0;
    let lo_v = (12 - lo) as f64 / 12.0;
    let (pair, suited) = (r == c, r < c);
    let mut p = 0.80 + 0.14 * hi_v + 0.06 * lo_v; // high cards realize
    if pair {
        p += 0.10; // made hands don't need to hit
    }
    if suited {
        p += 0.06;
    }
    if !pair && lo - hi <= 2 {
        p += 0.03; // connectedness
    }
    let position = if last_to_act { 1.06 } else { 0.94 };
    p * position * 0.96f64.powi(players.saturating_sub(2) as i32)
}

/// The seat that acts last postflop among `players`: the live seat closest
/// to the button (heads-up: the small blind is the button).
fn last_to_act(rs: &Ruleset, players: u8) -> usize {
    let n = rs.n();
    if n == 2 {
        return 0; // HU: SB = dealer acts last postflop
    }
    // Postflop position: SB(0) < BB(1) < UTG(2) < … < BTN(n-1).
    let pos = |s: usize| if s >= n - 2 { s - (n - 2) } else { s + 2 };
    (0..n)
        .filter(|s| players & (1 << s) != 0)
        .max_by_key(|&s| pos(s))
        .expect("non-empty live mask")
}

/// Values [`Terminal`]s for one hero seat given everyone's hand classes.
pub struct TerminalValuer {
    utility: Utility,
    rake_rate: f64,
    rake_cap_bb: f64,
    realization: bool,
}

impl TerminalValuer {
    /// Build from the ruleset (`icm_payouts` selects the utility).
    pub fn new(rs: &Ruleset) -> Self {
        Self {
            utility: match &rs.icm_payouts {
                Some(p) => Utility::Icm {
                    payouts: p.iter().map(|&x| f64::from(x)).collect(),
                },
                None => Utility::ChipEv,
            },
            rake_rate: f64::from(rs.rake_rate),
            rake_cap_bb: f64::from(rs.rake_cap_bb),
            realization: true,
        }
    }

    /// Value see-a-flop terminals as pure check-down equity (`R ≡ 1`) — the
    /// A/B baseline for the realization table.
    pub fn check_down(mut self) -> Self {
        self.realization = false;
        self
    }

    /// Hero's utility at terminal state `st`, given the hand's [`Deal`].
    /// Works for folded heroes too (their stake is fixed, but under ICM the
    /// pot's destination still moves their tournament equity).
    pub fn value(
        &self,
        rs: &Ruleset,
        st: &State,
        hero: usize,
        deal: &Deal,
        eq: &EquityCache,
        rng: &mut SmallRng,
    ) -> f64 {
        let t = st.terminal(rs).expect("value() wants a terminal state");
        let pot = st.pot(rs) as f64 / f64::from(CB);
        match t {
            // No flop, no drop: fold-wins are never raked.
            Terminal::FoldWin { winner } => {
                self.settle(rs, st, hero, &[(winner as usize, 1.0)], pot)
            }
            Terminal::AllInShowdown { players } => {
                let outcomes = self.pot_shares(rs, players, deal, eq, rng);
                self.settle(rs, st, hero, &outcomes, pot - self.rake(pot))
            }
            Terminal::SeeFlop { players } => {
                let mut outcomes = self.pot_shares(rs, players, deal, eq, rng);
                if self.realization {
                    let last = last_to_act(rs, players);
                    let k = players.count_ones();
                    for (seat, share) in &mut outcomes {
                        *share *= r_factor(deal.classes[*seat] as usize, *seat == last, k);
                    }
                }
                self.settle(rs, st, hero, &outcomes, pot - self.rake(pot))
            }
        }
    }

    fn rake(&self, pot: f64) -> f64 {
        (pot * self.rake_rate).min(self.rake_cap_bb)
    }

    /// (seat, pot-share) pairs for the live-mask showdown: exact class table
    /// heads-up, sampled runouts from the deal's remaining deck multiway.
    fn pot_shares(
        &self,
        rs: &Ruleset,
        players: u8,
        deal: &Deal,
        eq: &EquityCache,
        rng: &mut SmallRng,
    ) -> Vec<(usize, f64)> {
        let seats: Vec<usize> = (0..rs.n()).filter(|s| players & (1 << s) != 0).collect();
        if let [a, b] = seats[..] {
            let (ca, cb) = (deal.classes[a] as usize, deal.classes[b] as usize);
            return vec![(a, eq.hu(ca, cb)), (b, eq.hu(cb, ca))];
        }
        let holes: Vec<_> = seats.iter().map(|&s| deal.holes[s]).collect();
        let shares = sampled_shares(&holes, &deal.pool, eq.sample_boards, rng);
        seats.into_iter().zip(shares).collect()
    }

    /// Fold the (winner, share) outcomes into the hero's utility.
    ///
    /// Chip-EV takes the expectation directly. ICM evaluates Malmuth–Harville
    /// per *discrete* winner outcome and mixes by share — ties are folded
    /// into the share vector (documented ≈0.1% ceiling), and multi-street
    /// futures don't exist preflop, so the only `ICM(E[x]) ≈ E[ICM(x)]`
    /// approximation left is inside see-a-flop terminals.
    fn settle(
        &self,
        rs: &Ruleset,
        st: &State,
        hero: usize,
        outcomes: &[(usize, f64)],
        paid_pot: f64,
    ) -> f64 {
        let n = rs.n();
        let committed = |s: usize| st.committed[s] as f64 / f64::from(CB);
        match &self.utility {
            Utility::ChipEv => {
                let win: f64 = outcomes
                    .iter()
                    .filter(|(w, _)| *w == hero)
                    .map(|(_, share)| share * paid_pot)
                    .sum();
                win - committed(hero)
            }
            Utility::Icm { payouts } => {
                let base: Vec<f64> = (0..n)
                    .map(|s| f64::from(rs.stack_bb) - committed(s))
                    .collect();
                outcomes
                    .iter()
                    .map(|&(w, share)| {
                        let mut stacks = base.clone();
                        stacks[w] += paid_pot;
                        share * malmuth_harville(&stacks, payouts)[hero]
                    })
                    .sum()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::replay;
    use poker_trainer::preflop::CLASSES;
    use rand::SeedableRng;

    fn even_cache() -> EquityCache {
        EquityCache::new(vec![0.5; CLASSES * CLASSES])
    }

    #[test]
    fn malmuth_harville_matches_hand_computed_values() {
        // Hand-derived example, stacks 50/30/20, payouts 50/30/20. E.g. for
        // player 2: P(1st) = .3; P(2nd) = .5·30/50 + .2·30/80 = .375;
        // P(3rd) = .325 ⇒ 50·.3 + 30·.375 + 20·.325 = 32.75.
        let eq = malmuth_harville(&[50.0, 30.0, 20.0], &[50.0, 30.0, 20.0]);
        assert!((eq[0] - 38.392857).abs() < 1e-4, "{eq:?}");
        assert!((eq[1] - 32.75).abs() < 1e-4, "{eq:?}");
        assert!((eq[2] - 28.857143).abs() < 1e-4, "{eq:?}");
        // Equities exhaust the prize pool.
        assert!((eq.iter().sum::<f64>() - 100.0).abs() < 1e-9);
    }

    #[test]
    fn malmuth_harville_edges() {
        // Equal stacks ⇒ equal equity.
        let eq = malmuth_harville(&[40.0; 6], &[4.0, 2.0, 1.0, 0.0, 0.0, 0.0]);
        for e in &eq {
            assert!((e - 7.0 / 6.0).abs() < 1e-9, "{eq:?}");
        }
        // A felted player has zero equity; the rest still split it all.
        let eq = malmuth_harville(&[0.0, 60.0, 40.0], &[2.0, 1.0, 0.0]);
        assert_eq!(eq[0], 0.0);
        assert!((eq.iter().sum::<f64>() - 3.0).abs() < 1e-9);
        // Winner-take-all is just win probability × the prize.
        let eq = malmuth_harville(&[75.0, 25.0], &[10.0, 0.0]);
        assert!((eq[0] - 7.5).abs() < 1e-9);
    }

    #[test]
    fn realization_factors_bend_the_right_way() {
        let aa = 0;
        let so = poker_trainer::preflop::class_index_of("72o").unwrap();
        // Position: in position beats out of position for every class.
        assert!(r_factor(aa, true, 2) > r_factor(aa, false, 2));
        // Playability: AA realizes more than 72o everywhere.
        assert!(r_factor(aa, false, 2) > r_factor(so, false, 2));
        // Multiway squeezes realization down.
        assert!(r_factor(so, false, 4) < r_factor(so, false, 2));
        // Sane band overall.
        for class in 0..CLASSES {
            for (ip, k) in [(true, 2), (false, 2), (true, 6), (false, 6)] {
                let r = r_factor(class, ip, k);
                assert!((0.6..=1.25).contains(&r), "class {class}: {r}");
            }
        }
    }

    #[test]
    fn chip_ev_terminals_add_up() {
        let rs = crate::game::Ruleset::load(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../manifests/preflop/cash89.toml"
        ))
        .unwrap();
        // check_down: the even-split arithmetic below assumes R ≡ 1.
        let v = TerminalValuer::new(&rs).check_down();
        let eq = even_cache();
        let mut rng = SmallRng::seed_from_u64(1);
        let deal = Deal::class_level([0, 20, 40, 60, 80, 100]);

        // BB walk: the blinds move over, no rake.
        let st = replay(&rs, "f-f-f-f-f").unwrap();
        assert!((v.value(&rs, &st, 5, &deal, &eq, &mut rng) - 0.5).abs() < 1e-9);

        // BTN opens 2.5, BB calls, flop seen: pot 5.5, rake 5% = 0.275.
        // Even equity ⇒ each takes half the raked pot minus their 2.5 in.
        let st = replay(&rs, "f-f-f-r2.5-f-c").unwrap();
        let u = v.value(&rs, &st, 5, &deal, &eq, &mut rng);
        // 1e-6: the ruleset's f32 rake knobs wobble the f64 math slightly.
        assert!((u - (0.5 * (5.5 - 0.275) - 2.5)).abs() < 1e-6, "{u}");
    }

    #[test]
    fn icm_pressure_beats_chips() {
        // The defining ICM fact: at even stacks, a ~50/50 flip for stacks is
        // *negative* tournament equity though it's zero chip-EV. Inline 25bb
        // ICM ruleset (no shipped cash ruleset carries payouts); the 3-bet jam
        // is live (jam_from_level = 1).
        let rs: crate::game::Ruleset = toml::from_str(
            r#"
            id = "icm25"
            label = "6-max 25bb ICM"
            seats = ["UTG", "HJ", "CO", "BTN", "SB", "BB"]
            stack_bb = 25.0
            sb = 0.5
            bb = 1.0
            ante_bb = 1.0
            open_to_bb = [2.0, 2.5, 3.0]
            threebet_mult = [2.0, 3.0, 4.0]
            squeeze_mult = [4.0]
            fourbet_mult = [2.3]
            fivebet_mult = [2.2]
            jam_from_level = 1
            icm_payouts = [4.0, 2.0, 1.0, 0.0, 0.0, 0.0]
            [solver]
            traversals = 1000
            "#,
        )
        .unwrap();
        let v = TerminalValuer::new(&rs);
        let eq = even_cache();
        let mut rng = SmallRng::seed_from_u64(1);
        let deal = Deal::class_level([0, 20, 40, 60, 80, 100]);

        // Everyone's pre-hand tournament equity: payouts split evenly.
        let flat = 7.0 / 6.0;

        // UTG opens 2bb, HJ jams 25bb, folds back around, UTG calls the flip
        // (even 0.5 equity from the mock cache)…
        let st = replay(&rs, "r2-ai-f-f-f-f-c").unwrap();
        let u_call = v.value(&rs, &st, 0, &deal, &eq, &mut rng);
        assert!(u_call < flat, "flip {u_call} vs flat {flat}");
        // …or folds, and the jammer banks the pot uncontested.
        let st_fold = replay(&rs, "r2-ai-f-f-f-f-f").unwrap();
        let u_fold = v.value(&rs, &st_fold, 1, &deal, &eq, &mut rng);
        assert!(u_fold > flat, "steal {u_fold} vs flat {flat}");
    }
}
