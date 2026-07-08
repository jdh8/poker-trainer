//! The preflop betting game: a ruleset config plus a pure state machine.
//!
//! Nothing here allocates a tree — MCCFR walks states on the fly and the
//! exporter re-walks them top-down. All chip amounts are integer centi-bb so
//! pot math is exact. Seat order is act-first-first: index 0 opens the
//! action (UTG in 6-max, the SB in heads-up); the small blind is always seat
//! `n-2` and the big blind seat `n-1`.

use serde::Deserialize;
use std::path::Path;

/// Hard seat ceiling (the fixed-size arrays in [`State`]).
pub const MAX_SEATS: usize = 6;

/// Centi-bb per big blind.
pub const CB: u32 = 100;

/// Convert big blinds to centi-bb.
pub fn to_cb(bb: f32) -> u32 {
    (bb * CB as f32).round() as u32
}

/// Render centi-bb as a bb string with trailing zeros trimmed: `250` → `2.5`,
/// `300` → `3`, `1225` → `12.25`. Used by both path tokens and action labels.
pub fn fmt_bb(cb: u32) -> String {
    match (cb % CB, cb % 10) {
        (0, _) => (cb / CB).to_string(),
        (_, 0) => format!("{}.{}", cb / CB, (cb % CB) / 10),
        _ => format!("{}.{:02}", cb / CB, cb % CB),
    }
}

/// Solver knobs, per ruleset (`[solver]` in the TOML).
#[derive(Debug, Clone, Deserialize)]
pub struct SolverParams {
    /// MCCFR budget in dealt hands (each hand traverses once per seat).
    pub traversals: u64,
    /// RNG seed; identical seed + budget ⇒ identical output.
    #[serde(default = "default_seed")]
    pub seed: u64,
    /// Export nodes with equilibrium reach ≥ this (the full charts.jsonl).
    #[serde(default = "default_export_reach")]
    pub export_reach: f32,
    /// Commit nodes with reach ≥ this (the starter.jsonl tier).
    #[serde(default = "default_starter_reach")]
    pub starter_reach: f32,
}

fn default_seed() -> u64 {
    1
}
fn default_export_reach() -> f32 {
    0.001
}
fn default_starter_reach() -> f32 {
    0.05
}

/// Which seats may limp an unopened pot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LimpScope {
    /// No limps — fold or open only (the classic push/fold tree).
    None,
    /// Only the small blind may complete: one 2-way limped branch, no
    /// multiway limped pots. The cash-ladder default.
    Sb,
    /// Every unopened seat may limp (the full multiway limped tree).
    #[default]
    All,
}

/// One rule set: table format, blinds/antes, the raise-size menus, and the
/// optional ICM payout vector. Loaded from `manifests/preflop/<id>.toml`.
#[derive(Debug, Clone, Deserialize)]
pub struct Ruleset {
    /// Id — also the output directory name under `data/preflop/`.
    pub id: String,
    /// Human label for headers and the web ruleset picker.
    pub label: String,
    /// Seats in acting order; the last two post the blinds.
    pub seats: Vec<String>,
    /// Uniform effective stack, in bb.
    // ponytail: uniform stacks — all-in-for-less never happens and raise
    // reopening stays trivial; per-seat stacks are the upgrade for real
    // tournament states.
    pub stack_bb: f32,
    /// Small blind, in bb.
    pub sb: f32,
    /// Big blind (the unit everything else is measured in).
    pub bb: f32,
    /// Ante per player, dead in the pot (0 for cash; a tournament ruleset
    /// sets it).
    #[serde(default)]
    pub ante_bb: f32,
    /// Open-raise sizes, absolute bb ("raise to").
    pub open_to_bb: Vec<f32>,
    /// 3-bet sizes as multiples of the open, heads-up vs the opener.
    pub threebet_mult: Vec<f32>,
    /// 3-bet sizes as multiples of the open once the open has a caller.
    // ponytail: one squeeze size ships; the ceiling is chart nuance in
    // squeezed pots, the upgrade is widening this menu.
    pub squeeze_mult: Vec<f32>,
    /// 4-bet sizes as multiples of the 3-bet. One size + jam by design.
    pub fourbet_mult: Vec<f32>,
    /// 5-bet sizes as multiples of the 4-bet. One size + jam by design.
    /// Optional: an empty menu (the default) restores the jam-only 5-bet.
    #[serde(default)]
    pub fivebet_mult: Vec<f32>,
    /// Offer all-in as a raise option when facing raise-level ≥ this
    /// (0 = open-jams allowed, 2 = jam only vs a 3-bet or later). A 6-bet is
    /// always jam-only.
    pub jam_from_level: u8,
    /// Push/fold model: forbid limps (unopened = fold or raise) and skip the
    /// BB's option, so the tree matches the classic jam-or-fold game. Default
    /// off — cash rulesets allow limps. Shorthand for `limp_scope = "none"`
    /// (kept for the committed HU push/fold rulesets); `no_limps = true` wins.
    #[serde(default)]
    pub no_limps: bool,
    /// Which seats may complete an unopened pot (default `all`). `sb` keeps
    /// only the small blind's limp — one 2-way branch instead of the multiway
    /// limped pots that dominate the 6-max tree (design 07).
    #[serde(default)]
    pub limp_scope: LimpScope,
    /// Rake taken from the pot (0.05 = 5%). No flop, no drop: fold-win pots
    /// are never raked.
    #[serde(default)]
    pub rake_rate: f32,
    /// Rake cap in bb.
    #[serde(default)]
    pub rake_cap_bb: f32,
    /// ICM payout vector (one entry per seat, best finish first). `None` =
    /// chip-EV cash.
    #[serde(default)]
    pub icm_payouts: Option<Vec<f32>>,
    /// Solver knobs.
    pub solver: SolverParams,
}

impl Ruleset {
    /// Load and validate a ruleset TOML.
    pub fn load(path: impl AsRef<Path>) -> Result<Self, String> {
        let path = path.as_ref();
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("ruleset {}: {e}", path.display()))?;
        let rs: Ruleset =
            toml::from_str(&text).map_err(|e| format!("ruleset {}: {e}", path.display()))?;
        rs.validate()
            .map_err(|e| format!("ruleset {}: {e}", path.display()))?;
        Ok(rs)
    }

    /// Number of seats.
    pub fn n(&self) -> usize {
        self.seats.len()
    }

    /// Effective stack in centi-bb.
    pub fn stack(&self) -> u32 {
        to_cb(self.stack_bb)
    }

    /// Effective limp scope: `no_limps` forces `None`, else `limp_scope`.
    fn scope(&self) -> LimpScope {
        if self.no_limps {
            LimpScope::None
        } else {
            self.limp_scope
        }
    }

    /// May the seat at acting index `me` limp an unopened pot? (`sb` = the
    /// last non-BB seat completes; every other seat folds or opens.)
    fn may_limp(&self, me: usize) -> bool {
        match self.scope() {
            LimpScope::None => false,
            LimpScope::All => true,
            LimpScope::Sb => me == self.n() - 2,
        }
    }

    /// Do limps exist at all? Gates the BB's option in an unraised pot.
    fn limps_enabled(&self) -> bool {
        self.scope() != LimpScope::None
    }

    fn validate(&self) -> Result<(), String> {
        let n = self.n();
        if !(2..=MAX_SEATS).contains(&n) {
            return Err(format!("need 2..={MAX_SEATS} seats, got {n}"));
        }
        if let Some(p) = &self.icm_payouts {
            if p.len() != n {
                return Err(format!("icm_payouts has {} entries for {n} seats", p.len()));
            }
        }
        if self.stack_bb <= self.bb || self.sb <= 0.0 || self.bb <= 0.0 {
            return Err("need stack > bb and positive blinds".into());
        }
        if self.open_to_bb.iter().any(|&o| to_cb(o) <= to_cb(self.bb)) {
            return Err("every open size must exceed the big blind".into());
        }
        for (name, mults) in [
            ("threebet_mult", &self.threebet_mult),
            ("squeeze_mult", &self.squeeze_mult),
            ("fourbet_mult", &self.fourbet_mult),
            ("fivebet_mult", &self.fivebet_mult),
        ] {
            if mults.iter().any(|&m| m <= 1.0) {
                return Err(format!("{name} entries must be > 1.0"));
            }
        }
        Ok(())
    }
}

/// One player action. Raise amounts are "raise to", in centi-bb.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Give up the hand (and any blind already posted).
    Fold,
    /// Match the current bet (calling a jam commits the full stack). Limping
    /// is a call of the big blind in an unopened pot.
    Call,
    /// Pass with no chips in an unraised pot — the BB's option, or a check
    /// behind limpers. Never offered when facing a bet.
    Check,
    /// Raise to a fixed amount below the stack.
    RaiseTo(u32),
    /// Jam the full stack.
    AllIn,
}

impl Action {
    /// The action-path token: `f | c | x | r<to-bb> | ai` (design 07).
    pub fn token(&self) -> String {
        match self {
            Action::Fold => "f".into(),
            Action::Call => "c".into(),
            Action::Check => "x".into(),
            Action::RaiseTo(cb) => format!("r{}", fmt_bb(*cb)),
            Action::AllIn => "ai".into(),
        }
    }

    /// The pre-rendered display label the chart format carries.
    pub fn label(&self) -> String {
        match self {
            Action::Fold => "Fold".into(),
            Action::Call => "Call".into(),
            Action::Check => "Check".into(),
            Action::RaiseTo(cb) => format!("Raise to {}bb", fmt_bb(*cb)),
            Action::AllIn => "All-in".into(),
        }
    }
}

/// Marker for "betting closed" in [`State::actor`].
const CLOSED: u8 = MAX_SEATS as u8;

/// A betting state. `Copy`-small on purpose: MCCFR clones one per edge.
///
/// The infoset key hashes the whole public state, which merges histories
/// that differ only in when a never-invested seat folded.
// ponytail: public-state merging is benign imperfect recall (standard for
// preflop solvers); full-history keys are the ~3× memory fallback if it ever
// bites.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct State {
    /// Voluntarily committed chips (blinds count), centi-bb. Folded seats
    /// keep theirs — it stays in the pot.
    pub committed: [u32; MAX_SEATS],
    /// Bitmask of folded seats.
    pub folded: u8,
    /// Bitmask of all-in seats.
    pub all_in: u8,
    /// Current bet-to amount, centi-bb.
    pub cur_bet: u32,
    /// Raise level: 0 unopened, 1 open, 2 = 3-bet, 3 = 4-bet, 4 = 5-bet,
    /// 5 = 6-bet.
    pub level: u8,
    /// Someone called the open before a 3-bet (selects the squeeze menu).
    pub had_caller: bool,
    /// The BB still has its option: an unraised pot never returned to it.
    /// Limps make `committed == cur_bet` reachable mid-round, so the option
    /// can't be inferred from chips alone.
    bb_option: bool,
    /// Seat to act, or [`CLOSED`] when betting is over.
    actor: u8,
}

/// How a closed betting round resolves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Terminal {
    /// Everyone else folded; `winner` takes the pot unraked (no flop no drop).
    FoldWin {
        /// The last seat standing.
        winner: u8,
    },
    /// Two or more players all-in for the full stack; run out all five cards.
    AllInShowdown {
        /// Bitmask of live seats.
        players: u8,
    },
    /// Betting matched below stack depth; the pot sees a flop.
    SeeFlop {
        /// Bitmask of live seats.
        players: u8,
    },
}

impl State {
    /// The initial state: blinds posted, first seat to act.
    pub fn root(rs: &Ruleset) -> State {
        let n = rs.n();
        let mut committed = [0u32; MAX_SEATS];
        committed[n - 2] = to_cb(rs.sb);
        committed[n - 1] = to_cb(rs.bb);
        State {
            committed,
            folded: 0,
            all_in: 0,
            cur_bet: to_cb(rs.bb),
            level: 0,
            had_caller: false,
            bb_option: true,
            actor: 0,
        }
    }

    /// The seat to act, or `None` when betting is closed (see [`terminal`]).
    ///
    /// [`terminal`]: State::terminal
    pub fn to_act(self) -> Option<u8> {
        (self.actor != CLOSED).then_some(self.actor)
    }

    /// Bitmask of seats still in the hand.
    pub fn live(&self, rs: &Ruleset) -> u8 {
        !self.folded & ((1u16 << rs.n()) - 1) as u8
    }

    /// Total pot in centi-bb: all commitments (folded seats' included) plus
    /// every seat's dead ante.
    pub fn pot(&self, rs: &Ruleset) -> u32 {
        let n = rs.n();
        self.committed[..n].iter().sum::<u32>() + n as u32 * to_cb(rs.ante_bb)
    }

    /// How the closed round resolves; `None` while someone still acts.
    pub fn terminal(&self, rs: &Ruleset) -> Option<Terminal> {
        if self.actor != CLOSED {
            return None;
        }
        let live = self.live(rs);
        Some(if live.count_ones() == 1 {
            Terminal::FoldWin {
                winner: live.trailing_zeros() as u8,
            }
        } else if self.cur_bet >= rs.stack() {
            Terminal::AllInShowdown { players: live }
        } else {
            Terminal::SeeFlop { players: live }
        })
    }

    /// Legal actions for the acting seat, appended to `out` (cleared first).
    /// Facing a bet: fold or call, then the level's raises. Unraised and
    /// already matched (the BB's option): check or raise — no free fold.
    /// Everyone else unopened may fold, limp (call), or open. A walk still
    /// ends the hand when everyone folds to the BB (it never acts).
    // ponytail: a raise over limpers reuses the open menu (absolute bb); a
    // dedicated iso-size is the upgrade if limped-pot charts need it.
    pub fn legal(&self, rs: &Ruleset, out: &mut Vec<Action>) {
        out.clear();
        let me = self.actor as usize;
        let stack = rs.stack();
        if self.committed[me] >= self.cur_bet {
            out.push(Action::Check); // BB option in an unraised pot; no free fold
        } else {
            out.push(Action::Fold);
            if self.level > 0 || rs.may_limp(me) {
                out.push(Action::Call); // a limp at level 0, else facing a bet
            }
            if self.cur_bet >= stack {
                return; // facing a jam: fold or call only
            }
        }
        let mult = |m: &f32| ((self.cur_bet as f32) * m).round() as u32;
        let raises: Vec<u32> = match self.level {
            0 => rs.open_to_bb.iter().map(|&o| to_cb(o)).collect(),
            1 if self.had_caller => rs.squeeze_mult.iter().map(mult).collect(),
            1 => rs.threebet_mult.iter().map(mult).collect(),
            2 => rs.fourbet_mult.iter().map(mult).collect(),
            3 => rs.fivebet_mult.iter().map(mult).collect(),
            _ => vec![], // a 6-bet is jam-only
        };
        let mut jam = self.level >= rs.jam_from_level || self.level >= 4;
        for r in raises {
            debug_assert!(r > self.cur_bet, "raise menus must exceed the bet");
            if r >= stack {
                jam = true; // a menu size at/over the stack collapses into the jam
            } else if !out.contains(&Action::RaiseTo(r)) {
                out.push(Action::RaiseTo(r));
            }
        }
        if jam {
            out.push(Action::AllIn);
        }
    }

    /// Apply `a` for the acting seat and advance to the next actor (or close
    /// the betting).
    pub fn apply(&self, rs: &Ruleset, a: Action) -> State {
        let mut s = *self;
        let me = self.actor as usize;
        let stack = rs.stack();
        match a {
            Action::Fold => s.folded |= 1 << me,
            Action::Check => {} // the BB's option: no chips, no level change
            Action::Call => {
                s.committed[me] = self.cur_bet.min(stack);
                if s.committed[me] == stack {
                    s.all_in |= 1 << me;
                }
                if self.level == 1 {
                    s.had_caller = true;
                }
            }
            Action::RaiseTo(x) => {
                debug_assert!(x > self.cur_bet && x < stack);
                s.committed[me] = x;
                s.cur_bet = x;
                s.level += 1;
            }
            Action::AllIn => {
                s.committed[me] = stack;
                s.cur_bet = stack;
                s.level += 1;
                s.all_in |= 1 << me;
            }
        }

        // Next actor: the first live, not-all-in seat after `me` still owing
        // chips — plus the BB's one-time option in an unraised pot (it posted
        // `cur_bet` as a blind but never acted). Limps make `committed ==
        // cur_bet` reachable mid-round, so the option needs its own flag.
        let n = rs.n();
        let bb_seat = n - 1;
        if me == bb_seat {
            s.bb_option = false; // the BB has now acted
        }
        s.actor = CLOSED;
        if s.live(rs).count_ones() > 1 {
            for k in 1..=n {
                let seat = (me + k) % n;
                let bit = 1u8 << seat;
                let owes = s.committed[seat] < s.cur_bet;
                let bb_opt = seat == bb_seat && s.bb_option && s.level == 0 && rs.limps_enabled();
                if s.folded & bit == 0 && s.all_in & bit == 0 && (owes || bb_opt) {
                    s.actor = seat as u8;
                    break;
                }
            }
        }
        s
    }

    /// Stable FNV-1a key of the public state (the infoset key alongside the
    /// actor's hand class). Same hash idiom as `SpotConfig::hash8`.
    pub fn key(&self) -> u64 {
        let mut h: u64 = 0xcbf29ce484222325;
        let mut eat = |b: u8| {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x100000001b3);
        };
        eat(self.actor);
        eat(self.folded);
        eat(self.all_in);
        eat(self.level);
        eat(self.had_caller as u8);
        eat(self.bb_option as u8);
        for b in self.cur_bet.to_le_bytes() {
            eat(b);
        }
        for c in &self.committed {
            for b in c.to_le_bytes() {
                eat(b);
            }
        }
        h
    }
}

/// Walk `path` (`"f-f-r2.5-f-c"`) from the root; `Ok` holds the state (which
/// may be terminal). Rejects illegal tokens at any step.
pub fn replay(rs: &Ruleset, path: &str) -> Result<State, String> {
    let mut st = State::root(rs);
    let mut legal = Vec::new();
    for tok in path.split('-').filter(|t| !t.is_empty()) {
        if st.to_act().is_none() {
            return Err(format!("path continues past a terminal at {tok:?}"));
        }
        st.legal(rs, &mut legal);
        let a = *legal
            .iter()
            .find(|a| a.token() == tok)
            .ok_or_else(|| format!("illegal token {tok:?} (legal: {legal:?})"))?;
        st = st.apply(rs, a);
    }
    Ok(st)
}

/// Tree statistics from a full DFS — the `tree` subcommand and the node-count
/// regression test.
#[derive(Debug, Default, PartialEq)]
pub struct TreeStats {
    /// Decision histories (one per acting node, pre-merge).
    pub decisions: u64,
    /// Distinct public-state keys (post-merge infoset states).
    pub states: u64,
    /// Action edges out of decision histories.
    pub edges: u64,
    /// Fold-win terminals (walks included).
    pub fold_wins: u64,
    /// All-in showdowns, split heads-up vs multiway.
    pub allin_2way: u64,
    /// All-in showdowns with 3+ players.
    pub allin_multi: u64,
    /// Flop-seeing terminals, split heads-up vs multiway.
    pub flop_2way: u64,
    /// Flop-seeing terminals with 3+ players.
    pub flop_multi: u64,
    /// Longest action sequence.
    pub max_depth: u32,
}

/// DFS the whole game and count (see [`TreeStats`]).
pub fn tree_stats(rs: &Ruleset) -> TreeStats {
    let mut stats = TreeStats::default();
    let mut keys = std::collections::HashSet::new();
    walk(rs, State::root(rs), 0, &mut stats, &mut keys);
    stats.states = keys.len() as u64;
    stats
}

fn walk(
    rs: &Ruleset,
    st: State,
    depth: u32,
    stats: &mut TreeStats,
    keys: &mut std::collections::HashSet<u64>,
) {
    stats.max_depth = stats.max_depth.max(depth);
    if let Some(t) = st.terminal(rs) {
        match t {
            Terminal::FoldWin { .. } => stats.fold_wins += 1,
            Terminal::AllInShowdown { players } if players.count_ones() == 2 => {
                stats.allin_2way += 1
            }
            Terminal::AllInShowdown { .. } => stats.allin_multi += 1,
            Terminal::SeeFlop { players } if players.count_ones() == 2 => stats.flop_2way += 1,
            Terminal::SeeFlop { .. } => stats.flop_multi += 1,
        }
        return;
    }
    stats.decisions += 1;
    keys.insert(st.key());
    let mut acts = Vec::new();
    st.legal(rs, &mut acts);
    stats.edges += acts.len() as u64;
    for a in acts {
        walk(rs, st.apply(rs, a), depth + 1, stats, keys);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest(name: &str) -> Ruleset {
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../../manifests/preflop");
        Ruleset::load(format!("{dir}/{name}.toml")).unwrap()
    }

    /// A 6-max cash ruleset with all-seat limps and the full {2,3,4}× 3-bet
    /// menu — exercises the multiway limp-around and menu machinery that the
    /// shipped `sb`-scope ladder no longer uses. (Was the old cash100 shape.)
    fn cash_6max_all() -> Ruleset {
        toml::from_str(
            r#"
            id = "cashall"
            label = "6-max cash, all limps"
            seats = ["UTG", "HJ", "CO", "BTN", "SB", "BB"]
            stack_bb = 100.0
            sb = 0.5
            bb = 1.0
            open_to_bb = [2.0, 2.5, 3.0]
            threebet_mult = [2.0, 3.0, 4.0]
            squeeze_mult = [4.0]
            fourbet_mult = [2.3]
            fivebet_mult = [2.2]
            jam_from_level = 2
            [solver]
            traversals = 1000
            "#,
        )
        .unwrap()
    }

    /// A 6-max ruleset with a dead ante — no shipped cash ruleset has one,
    /// so ante/pot math is covered inline.
    fn six_max_ante() -> Ruleset {
        toml::from_str(
            r#"
            id = "ante6"
            label = "6-max 40bb, 0.25 ante"
            seats = ["UTG", "HJ", "CO", "BTN", "SB", "BB"]
            stack_bb = 40.0
            sb = 0.5
            bb = 1.0
            ante_bb = 0.25
            open_to_bb = [2.0, 2.5, 3.0]
            threebet_mult = [2.0, 3.0, 4.0]
            squeeze_mult = [4.0]
            fourbet_mult = [2.3]
            fivebet_mult = [2.2]
            jam_from_level = 2
            [solver]
            traversals = 1000
            "#,
        )
        .unwrap()
    }

    /// A tiny hand-checkable ruleset: HU, 10bb, jam-or-fold.
    fn hu_pushfold() -> Ruleset {
        toml::from_str(
            r#"
            id = "hu10"
            label = "HU 10bb push/fold"
            seats = ["SB", "BB"]
            stack_bb = 10.0
            sb = 0.5
            bb = 1.0
            open_to_bb = []
            threebet_mult = [3.0]
            squeeze_mult = [3.0]
            fourbet_mult = [2.3]
            jam_from_level = 0
            [solver]
            traversals = 1000
            "#,
        )
        .unwrap()
    }

    #[test]
    fn fmt_bb_trims_trailing_zeros() {
        assert_eq!(fmt_bb(300), "3");
        assert_eq!(fmt_bb(250), "2.5");
        assert_eq!(fmt_bb(1225), "12.25");
        assert_eq!(fmt_bb(50), "0.5");
    }

    #[test]
    fn hu_pushfold_limp_and_option_tree() {
        let rs = hu_pushfold();
        let stats = tree_stats(&rs);
        assert_eq!(
            stats,
            TreeStats {
                decisions: 4, // SB fold/limp/jam, BB option, SB-vs-jam, BB-vs-jam
                states: 4,
                edges: 9,
                fold_wins: 3,  // SB folds; limp→BB-jam→SB folds; SB jams→BB folds
                allin_2way: 2, // limp→BB jam→SB call; SB jam→BB call
                allin_multi: 0,
                flop_2way: 1, // SB limps, BB checks its option
                flop_multi: 0,
                max_depth: 3,
            }
        );

        // Walk the jam-call line by hand.
        let st = replay(&rs, "ai-c").unwrap();
        assert_eq!(
            st.terminal(&rs),
            Some(Terminal::AllInShowdown { players: 0b11 })
        );
        assert_eq!(st.pot(&rs), 2000); // both stacks
        let walk_st = replay(&rs, "f").unwrap();
        assert_eq!(walk_st.terminal(&rs), Some(Terminal::FoldWin { winner: 1 }));
        assert_eq!(walk_st.pot(&rs), 150); // SB's dead 0.5 + BB's 1.0
    }

    #[test]
    fn pot_counts_antes_and_dead_blinds() {
        let rs = six_max_ante();
        // Root pot: 0.5 + 1.0 blinds + 6 × 0.25 ante = 3bb.
        assert_eq!(State::root(&rs).pot(&rs), 300);
        // UTG opens 2.5bb, HJ folds: their commitments both stay in the pot.
        let st = replay(&rs, "r2.5-f").unwrap();
        assert_eq!(st.pot(&rs), 550);
        assert_eq!(st.to_act(), Some(2));
    }

    #[test]
    fn legality_follows_the_level_menus() {
        let rs = cash_6max_all();
        let mut acts = Vec::new();

        // Unopened: fold, limp, or open — no jam (jam_from_level = 2).
        State::root(&rs).legal(&rs, &mut acts);
        assert_eq!(
            acts,
            vec![
                Action::Fold,
                Action::Call,
                Action::RaiseTo(200),
                Action::RaiseTo(250),
                Action::RaiseTo(300)
            ]
        );

        // Limped around to the BB: its option is check or raise, never a
        // free fold. jam_from_level = 2, so no open-jam here.
        let st = replay(&rs, "c-c-c-c-c").unwrap();
        assert_eq!(st.to_act(), Some(5)); // BB
        st.legal(&rs, &mut acts);
        assert_eq!(
            acts,
            vec![
                Action::Check,
                Action::RaiseTo(200),
                Action::RaiseTo(250),
                Action::RaiseTo(300)
            ]
        );

        // Facing a 2bb open heads-up: 3-bet menu is 2/3/4 × the open.
        let st = replay(&rs, "r2-f-f-f-f").unwrap();
        assert_eq!(st.to_act(), Some(5)); // BB
        st.legal(&rs, &mut acts);
        assert_eq!(
            acts,
            vec![
                Action::Fold,
                Action::Call,
                Action::RaiseTo(400),
                Action::RaiseTo(600),
                Action::RaiseTo(800)
            ]
        );

        // Facing the open with a caller behind: the squeeze menu applies.
        let st = replay(&rs, "r2-c-f-f-f").unwrap();
        st.legal(&rs, &mut acts);
        assert_eq!(acts, vec![Action::Fold, Action::Call, Action::RaiseTo(800)]);

        // Facing a 3-bet (level 2): 4-bet size + jam appears (jam_from_level).
        let st = replay(&rs, "r2.5-f-f-r7.5-f-f").unwrap();
        assert_eq!(st.to_act(), Some(0)); // back on the opener
        st.legal(&rs, &mut acts);
        assert_eq!(
            acts,
            vec![
                Action::Fold,
                Action::Call,
                Action::RaiseTo(1725), // 2.3 × 7.5bb
                Action::AllIn
            ]
        );

        // Facing a 4-bet (level 3): the sized 5-bet + jam appears.
        let st = replay(&rs, "r2.5-f-f-r7.5-f-f-r17.25").unwrap();
        st.legal(&rs, &mut acts);
        assert_eq!(
            acts,
            vec![
                Action::Fold,
                Action::Call,
                Action::RaiseTo(3795), // 2.2 × 17.25bb
                Action::AllIn
            ]
        );

        // Facing a 5-bet (level 4): a 6-bet is jam-only. Facing the jam: fold/call.
        let st = replay(&rs, "r2.5-f-f-r7.5-f-f-r17.25-r37.95").unwrap();
        st.legal(&rs, &mut acts);
        assert_eq!(acts, vec![Action::Fold, Action::Call, Action::AllIn]);
        let st = replay(&rs, "r2.5-f-f-r7.5-f-f-r17.25-r37.95-ai").unwrap();
        st.legal(&rs, &mut acts);
        assert_eq!(acts, vec![Action::Fold, Action::Call]);
    }

    #[test]
    fn walks_and_multiway_terminals_resolve() {
        let rs = cash_6max_all();
        // Everyone folds: the BB walks without ever acting.
        let st = replay(&rs, "f-f-f-f-f").unwrap();
        assert_eq!(st.terminal(&rs), Some(Terminal::FoldWin { winner: 5 }));

        // Open, two callers, BB comes along: a 4-way flop.
        let st = replay(&rs, "r2.5-c-c-f-f-c").unwrap();
        assert_eq!(
            st.terminal(&rs),
            Some(Terminal::SeeFlop { players: 0b100111 })
        );
        assert_eq!(st.pot(&rs), 1050); // 4 × 2.5bb + SB's dead 0.5
    }

    #[test]
    fn replay_rejects_illegal_tokens() {
        let rs = cash_6max_all();
        assert!(replay(&rs, "c").is_ok()); // a limp is now legal
        assert!(replay(&rs, "x").is_err()); // UTG can't check unopened, only limp
        assert!(replay(&rs, "r5").is_err()); // not a menu size
        assert!(replay(&rs, "f-f-f-f-f-f").is_err()); // past the walk
    }

    #[test]
    fn public_state_key_merges_fold_order() {
        let rs = cash_6max_all();
        // CO opens after UTG and HJ folded — same public state for the BTN
        // regardless of the (nonexistent) fold-order variation; sanity: the
        // key at least distinguishes different states.
        let a = replay(&rs, "f-f-r2.5").unwrap();
        let b = replay(&rs, "f-f-r3").unwrap();
        assert_ne!(a.key(), b.key());
        assert_eq!(a.key(), replay(&rs, "f-f-r2.5").unwrap().key());
    }

    #[test]
    fn shipped_manifests_load_and_validate() {
        // Fibonacci depth ladder, shifted one rung up from the HU set (design 07).
        for id in [
            "cash5", "cash8", "cash13", "cash21", "cash34", "cash55", "cash89", "cash144",
        ] {
            let rs = manifest(id);
            assert_eq!(rs.id, id);
            assert_eq!(rs.n(), 6);
            assert!(rs.icm_payouts.is_none()); // all cash: chip-EV
            assert_eq!(rs.limp_scope, LimpScope::Sb); // SB-only limps (design 07)
            assert_eq!(rs.threebet_mult, vec![3.0, 4.0]); // trimmed 3-bet menu
        }
        // "Jams earlier" ladder: push/fold short, jam-vs-open mid, jam-vs-3bet deep.
        assert_eq!(manifest("cash5").jam_from_level, 0);
        assert_eq!(manifest("cash21").jam_from_level, 0);
        assert_eq!(manifest("cash34").jam_from_level, 1);
        assert_eq!(manifest("cash89").jam_from_level, 2);
        assert_eq!(manifest("cash144").jam_from_level, 2);
    }

    #[test]
    fn sb_scope_gates_limps_and_trims_the_3bet_menu() {
        let rs = manifest("cash89");
        assert_eq!(rs.limp_scope, LimpScope::Sb);
        let mut acts = Vec::new();

        // UTG unopened: fold or open — no limp under sb scope.
        State::root(&rs).legal(&rs, &mut acts);
        assert_eq!(
            acts,
            vec![
                Action::Fold,
                Action::RaiseTo(200),
                Action::RaiseTo(250),
                Action::RaiseTo(300)
            ]
        );
        // An early limp is unreachable (no multiway limped pots).
        assert!(replay(&rs, "c").is_err());

        // Folded to the SB: it alone may complete (fold/limp/open).
        let st = replay(&rs, "f-f-f-f").unwrap();
        assert_eq!(st.to_act(), Some(4)); // SB
        st.legal(&rs, &mut acts);
        assert_eq!(
            acts,
            vec![
                Action::Fold,
                Action::Call, // the SB limp
                Action::RaiseTo(200),
                Action::RaiseTo(250),
                Action::RaiseTo(300)
            ]
        );

        // SB limps: the BB still gets its option (check or raise), heads-up.
        let st = replay(&rs, "f-f-f-f-c").unwrap();
        assert_eq!(st.to_act(), Some(5)); // BB
        st.legal(&rs, &mut acts);
        assert_eq!(
            acts,
            vec![
                Action::Check,
                Action::RaiseTo(200),
                Action::RaiseTo(250),
                Action::RaiseTo(300)
            ]
        );

        // 3-bet menu trimmed to 3/4 × the open (a 2bb open → 6/8bb).
        let st = replay(&rs, "r2-f-f-f-f").unwrap();
        st.legal(&rs, &mut acts);
        assert_eq!(
            acts,
            vec![
                Action::Fold,
                Action::Call,
                Action::RaiseTo(600),
                Action::RaiseTo(800)
            ]
        );
    }

    /// Regression pin for the shipped trees (a representative subset of the
    /// cash depth ladder). If a rule change moves these numbers, that's a
    /// *deliberate* re-solve of every ruleset — update the pins and the
    /// design-07 table together. SB-only limps keep just the one 2-way
    /// completed branch, so the multiway limped pots are gone (design 07).
    #[test]
    fn shipped_tree_counts_are_pinned() {
        // cash89 and cash144 share this exact shape — no raise size reaches
        // either stack, so only the equilibrium differs.
        let deep = TreeStats {
            decisions: 885_384,
            states: 311_682,
            edges: 1_907_869,
            fold_wins: 137_107,
            allin_2way: 304_650,
            allin_multi: 460_644,
            flop_2way: 45_361,
            flop_multi: 74_724,
            max_depth: 26,
        };
        assert_eq!(tree_stats(&manifest("cash89")), deep);
        assert_eq!(tree_stats(&manifest("cash144")), deep);
        // cash55 shares jam_from_level = 2 but is smaller: the 5-bet sizes
        // collapse into the jam at 55bb.
        assert_eq!(
            tree_stats(&manifest("cash55")),
            TreeStats {
                decisions: 673_940,
                states: 254_423,
                edges: 1_451_617,
                fold_wins: 103_743,
                allin_2way: 231_258,
                allin_multi: 351_698,
                flop_2way: 34_209,
                flop_multi: 56_770,
                max_depth: 26,
            }
        );
        assert_eq!(
            tree_stats(&manifest("cash34")),
            TreeStats {
                decisions: 175_394,
                states: 105_715,
                edges: 376_051,
                fold_wins: 25_269,
                allin_2way: 58_466,
                allin_multi: 94_580,
                flop_2way: 7_973,
                flop_multi: 14_370,
                max_depth: 22,
            }
        );
        assert_eq!(
            tree_stats(&manifest("cash13")),
            TreeStats {
                decisions: 18_832,
                states: 16_936,
                edges: 40_017,
                fold_wins: 2_359,
                allin_2way: 5_866,
                allin_multi: 10_848,
                flop_2way: 673,
                flop_multi: 1_440,
                max_depth: 16,
            }
        );
    }
}
