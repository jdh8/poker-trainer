//! Export the value-net training corpus (design doc 09, phase a) from the
//! reach-pruned postflop tables: one fixed-width record per stored **turn
//! root** — the depth-limited leaf interface `V(board, pot, both ranges) →
//! per-hand counterfactual values`.
//!
//! A turn root is a stored node whose line ends with a `deal` token (always
//! OOP to act). Per root this tool reconstructs both players' pure reach
//! vectors by walking the line prefixes (header ranges → per-step action
//! frequencies, board-blocked combos zeroed), takes OOP counterfactual values
//! directly from the stored mix (`Σ_a freq·ev`), and rolls IP counterfactual
//! values back from the stored IP children with card-removal-correct mixing
//! weights. The rollback is exact only when **every** root action has a
//! stored child; at low-reach roots a pruned child can carry large
//! conditional probability (measured up to ~20% of the pot), so incomplete
//! roots keep their OOP side but mask the IP side (flags bit0 = 0) — ~66% of
//! roots are complete on the srp tiers.
//!
//! Record layout (little-endian, `RECORD_BYTES` total, echoed in
//! `corpus.json`): `u16 flop_id, u8 formation_id, u8 flags (bit0 = IP values
//! present), u8[4] board ids ([3] = turn), f32 pot_bb, f32 line reach,` then
//! four `f16[1326]` blocks: OOP reach, IP reach, OOP cfv/pot, IP cfv/pot.
//! Combo slots use card ids `rank*4 + suit` in `cdhs` order (2c=0 … As=51),
//! combo `(hi, lo)` with `hi > lo` at index `hi*(hi-1)/2 + lo`. CFVs use the
//! solver's convention (fold = 0); training masks slots with zero reach.
//! Validation flops (`fnv1a64(flop) % 10 == 0`) also append OOP equity per
//! combo to a sidecar shard — the eval baseline.
//!
//! Two invariants are checked on every root and fail the run loudly:
//! stored `weights` must equal `own_reach × unblocked-opponent-mass` up to
//! one global scale, and the reach-weighted averages of both cfv sides must
//! sum to the pot (measured ≤ 0.0002 bb on real tables).

use clap::Parser;
use poker_trainer::iso;
use poker_trainer::postflop_table::{PostflopTable, TableHeader, TableNode};
use poker_trainer::report::parse_range;
use poker_trainer::solution::FORMATIONS;
use rs_poker::core::{Card, Suit};
use serde::Serialize;
use std::collections::HashMap;
use std::fs;
use std::io::{BufWriter, Write};
use std::path::PathBuf;

/// Combo slots: 52 choose 2.
const N_COMBOS: usize = 52 * 51 / 2;
/// Fixed record width: 16-byte header + 4 × f16[N_COMBOS].
const RECORD_BYTES: usize = 16 + 4 * 2 * N_COMBOS;
/// Every 10th flop by hash is held out for validation.
const VAL_MOD: u64 = 10;
/// Stored weights are ~4-decimal JSON: a 1% (of the max weight) residue after
/// removing the global scale flags a structural bug, not rounding.
const WEIGHT_DEV_TOL: f32 = 0.01;
/// Measured zero-sum drift on real tables is ≤ 0.0002 bb; 0.3% of the pot
/// (floored at 0.02 bb) flags real bugs with two orders of headroom.
const ZERO_SUM_TOL_BB: f32 = 0.02;

#[derive(Parser)]
#[command(
    about = "Export the design-09 value-net corpus from reach-pruned postflop tables",
    long_about = None
)]
struct Args {
    /// Table store root (formation dirs live under it).
    #[arg(long, default_value = "data/tables")]
    tables: PathBuf,
    /// Output directory for the .bin shards + corpus.json.
    #[arg(long)]
    out: PathBuf,
    /// Comma-separated formation dirs (default: every curated formation
    /// present under --tables).
    #[arg(long)]
    formations: Option<String>,
    /// Cap canonical flops per formation (smoke tests).
    #[arg(long)]
    limit: Option<usize>,
}

fn main() {
    let args = Args::parse();
    match run(&args) {
        Ok(true) => {}
        Ok(false) => {
            eprintln!("invariant breaches — corpus written but NOT trustworthy");
            std::process::exit(1);
        }
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(2);
        }
    }
}

// ---- card / combo math ------------------------------------------------------

/// Index of combo `(hi, lo)`, `hi > lo`, in the 1326-slot triangular order.
fn combo_index(hi: u8, lo: u8) -> usize {
    debug_assert!(hi > lo);
    hi as usize * (hi as usize - 1) / 2 + lo as usize
}

/// `(hi, lo, index)` of a solver hand string like `"AsKs"`.
fn combo_of(hand: &str) -> Option<(u8, u8, usize)> {
    if !hand.is_ascii() || hand.len() != 4 {
        return None;
    }
    let a = iso::card_id(&hand[..2])?;
    let b = iso::card_id(&hand[2..])?;
    if a == b {
        return None;
    }
    let (hi, lo) = (a.max(b), a.min(b));
    Some((hi, lo, combo_index(hi, lo)))
}

/// rs_poker `Card` → the table card id (`rank*4 + suit`, suits in cdhs order).
fn card_id_of(c: Card) -> u8 {
    let suit = match c.suit {
        Suit::Club => 0,
        Suit::Diamond => 1,
        Suit::Heart => 2,
        Suit::Spade => 3,
    };
    (c.value as u8) * 4 + suit
}

/// Solver range syntax → per-combo weights: comma-separated `class[:weight]`
/// tokens (weight defaults to 1), classes expanded by [`parse_range`].
fn parse_weighted_range(s: &str) -> Result<Vec<f32>, String> {
    let mut w = vec![0f32; N_COMBOS];
    for tok in s.split(',').map(str::trim).filter(|t| !t.is_empty()) {
        let (class, weight) = match tok.rsplit_once(':') {
            Some((c, wt)) => (
                c,
                wt.trim()
                    .parse::<f32>()
                    .map_err(|_| format!("bad weight in range token {tok:?}"))?,
            ),
            None => (tok, 1.0),
        };
        for combo in parse_range(class)? {
            let (a, b) = (card_id_of(combo[0]), card_id_of(combo[1]));
            w[combo_index(a.max(b), a.min(b))] = weight;
        }
    }
    Ok(w)
}

/// FNV-1a 64-bit — the val-split hash (stable across platforms and runs).
fn fnv1a64(s: &str) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    for b in s.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

// ---- f16 --------------------------------------------------------------------

/// f32 → IEEE 754 binary16 bits, round-to-nearest-even (stable Rust has no
/// `f16`; 30 lines beat a dependency).
fn f16_bits(x: f32) -> u16 {
    let b = x.to_bits();
    let sign = ((b >> 16) & 0x8000) as u16;
    let exp = ((b >> 23) & 0xff) as i32;
    let man = b & 0x007f_ffff;
    if exp == 0xff {
        return sign | 0x7c00 | (u16::from(man != 0) * 0x200); // inf / quiet NaN
    }
    let e = exp - 127 + 15;
    if e >= 0x1f {
        return sign | 0x7c00; // overflow → inf
    }
    let (man16, rem, half) = if e > 0 {
        (((e as u32) << 10) | (man >> 13), man & 0x1fff, 0x1000u32)
    } else {
        // Subnormal: restore the implicit bit, shift out 13 + (1 - e) bits.
        let shift = 13 + (1 - e) as u32;
        if shift > 24 {
            return sign;
        }
        let man = man | 0x0080_0000;
        (man >> shift, man & ((1 << shift) - 1), 1 << (shift - 1))
    };
    let mut out = sign | man16 as u16;
    if rem > half || (rem == half && out & 1 == 1) {
        out += 1; // RNE; a mantissa carry into the exponent is correct
    }
    out
}

/// binary16 bits → f32 — the decode half of the shard contract (tests use
/// it; Python mirrors it when reading the shards).
#[cfg_attr(not(test), expect(dead_code))]
fn f16_val(h: u16) -> f32 {
    let sign = if h & 0x8000 != 0 { -1.0f32 } else { 1.0 };
    let exp = (h >> 10) & 0x1f;
    let man = u32::from(h & 0x3ff);
    match exp {
        0 => sign * man as f32 / (1u32 << 24) as f32,
        0x1f if man == 0 => sign * f32::INFINITY,
        0x1f => f32::NAN,
        _ => {
            let bits =
                ((u32::from(h) & 0x8000) << 16) | ((u32::from(exp) + 112) << 23) | (man << 13);
            f32::from_bits(bits)
        }
    }
}

// ---- card-removal sums ------------------------------------------------------

/// Per-card sums of a per-combo mass vector: `compat(h)` — the opponent mass
/// not blocked by hand `h` — in O(1) by inclusion-exclusion instead of an
/// O(n²) combo-vs-combo scan.
struct RemovalSums {
    total: f32,
    by_card: [f32; 52],
}

fn removal_sums(mass: &[f32], combos: &[(u8, u8, usize)]) -> RemovalSums {
    let mut total = 0f32;
    let mut by_card = [0f32; 52];
    for &(hi, lo, ix) in combos {
        let m = mass[ix];
        if m > 0.0 {
            total += m;
            by_card[hi as usize] += m;
            by_card[lo as usize] += m;
        }
    }
    RemovalSums { total, by_card }
}

impl RemovalSums {
    /// Mass compatible with holding `(hi, lo)`: total − both card sums, plus
    /// the doubly-removed identical combo back in. Clamped: f32 cancellation.
    fn compat(&self, mass: &[f32], hi: u8, lo: u8, ix: usize) -> f32 {
        (self.total - self.by_card[hi as usize] - self.by_card[lo as usize] + mass[ix]).max(0.0)
    }
}

// ---- per-file extraction ----------------------------------------------------

/// One turn root, pre-quantization; `write_record` fixes the byte layout.
struct RootRecord {
    board: [u8; 4],
    pot_bb: f32,
    reach: f32,
    flags: u8,
    oop_reach: Vec<f32>,
    ip_reach: Vec<f32>,
    oop_cfv_pot: Vec<f32>,
    ip_cfv_pot: Vec<f32>,
    /// Stored OOP equity scattered to combo slots (val-baseline sidecar).
    oop_equity: Vec<f32>,
}

#[derive(Default)]
struct FileStats {
    roots: usize,
    /// Roots with a pruned (unstored) child — IP side masked via flags.
    ip_masked: usize,
    /// IP hands whose mixing mass was zero (unweighted mean fallback).
    denom_zero: usize,
    /// Malformed / unexpected nodes skipped (any is a format-drift failure).
    bad_nodes: usize,
    weight_breaches: usize,
    zero_sum_breaches: usize,
    max_weight_dev: f32,
    max_zero_sum_dev_bb: f32,
    /// The line of the worst zero-sum root and how many of its actions had
    /// stored children — the first thing to look at when the check fails.
    worst_zero_sum: Option<(String, usize, usize)>,
}

/// Shape guard: parallel `freqs`/`evs` rows, one per action, `n` hands each.
fn well_formed(node: &TableNode, n: usize) -> bool {
    let t = &node.node;
    t.freqs.len() == t.actions.len()
        && t.evs.len() == t.actions.len()
        && t.freqs.iter().chain(t.evs.iter()).all(|r| r.len() == n)
}

fn extract_file(
    table: &PostflopTable,
    base_oop: &[f32],
    base_ip: &[f32],
) -> (Vec<RootRecord>, FileStats) {
    let mut stats = FileStats::default();
    let mut out = Vec::new();

    // The hands arrays are constant per player per file; index them once.
    let hands_of = |player: &str| {
        table
            .nodes()
            .find(|n| n.node.player == player)
            .map(|n| &n.node.hands)
    };
    let index_side = |hands: &[String]| {
        hands
            .iter()
            .map(|h| combo_of(h))
            .collect::<Option<Vec<_>>>()
    };
    let (Some(oop_hands), Some(ip_hands)) = (hands_of("oop"), hands_of("ip")) else {
        stats.bad_nodes += 1;
        return (out, stats);
    };
    let (Some(oop_combos), Some(ip_combos)) = (index_side(oop_hands), index_side(ip_hands)) else {
        stats.bad_nodes += 1;
        return (out, stats);
    };

    let mut roots: Vec<&TableNode> = table
        .nodes()
        .filter(|n| n.node.line.last().is_some_and(|l| l.starts_with("deal ")))
        .collect();
    roots.sort_by(|a, b| a.node.line.cmp(&b.node.line));

    'roots: for root in roots {
        let n = &root.node;
        let same_hands = n.hands.len() == oop_combos.len()
            && n.hands.first() == oop_hands.first()
            && n.hands.last() == oop_hands.last();
        if n.player != "oop"
            || n.board.len() != 4
            || n.actions.is_empty()
            || !same_hands
            || !well_formed(root, oop_combos.len())
            || n.weights.len() != oop_combos.len()
        {
            stats.bad_nodes += 1;
            continue;
        }
        let mut board = [0u8; 4];
        for (slot, card) in board.iter_mut().zip(&n.board) {
            let Some(id) = iso::card_id(card) else {
                stats.bad_nodes += 1;
                continue 'roots;
            };
            *slot = id;
        }

        // Reach vectors: header ranges, board-blocked zeroed, then one
        // frequency multiplication per betting step along the line.
        let mut rho_oop = base_oop.to_vec();
        let mut rho_ip = base_ip.to_vec();
        for &c in &board {
            for o in 0..52u8 {
                if o != c {
                    let ix = combo_index(c.max(o), c.min(o));
                    rho_oop[ix] = 0.0;
                    rho_ip[ix] = 0.0;
                }
            }
        }
        for j in 0..n.line.len() {
            let label = &n.line[j];
            if label.starts_with("deal ") {
                continue;
            }
            let Some(anc) = table.node(&n.line[..j]) else {
                stats.bad_nodes += 1;
                continue 'roots;
            };
            let (rho, combos) = match anc.node.player.as_str() {
                "oop" => (&mut rho_oop, &oop_combos),
                "ip" => (&mut rho_ip, &ip_combos),
                _ => {
                    stats.bad_nodes += 1;
                    continue 'roots;
                }
            };
            let taken = anc.node.actions.iter().position(|a| a == label);
            let Some(ai) = taken.filter(|_| well_formed(anc, combos.len())) else {
                stats.bad_nodes += 1;
                continue 'roots;
            };
            for (p, &(_, _, ix)) in combos.iter().enumerate() {
                rho[ix] *= anc.node.freqs[ai][p];
            }
        }

        // OOP counterfactual values: the stored equilibrium mix at the root.
        let mut oop_cfv = vec![0f32; N_COMBOS];
        for (p, &(_, _, ix)) in oop_combos.iter().enumerate() {
            oop_cfv[ix] = (0..n.actions.len())
                .map(|a| n.freqs[a][p] * n.evs[a][p])
                .sum();
        }

        // IP counterfactual values, rolled back from the stored IP children.
        // Mixing weight per IP hand h and OOP action a: the OOP mass taking
        // `a` that h doesn't block. Exact only when every action's child is
        // stored — a pruned child can carry large conditional probability at
        // low-reach roots, so incomplete roots mask the IP side instead.
        let children: Vec<(usize, &TableNode)> = n
            .actions
            .iter()
            .enumerate()
            .filter_map(|(ai, a)| {
                let mut line = n.line.clone();
                line.push(a.clone());
                let c = table.node(&line)?;
                (c.node.player == "ip" && well_formed(c, ip_combos.len())).then_some((ai, c))
            })
            .collect();
        let mut ip_cfv = vec![0f32; N_COMBOS];
        let mut flags = 0u8;
        if children.len() < n.actions.len() {
            stats.ip_masked += 1;
        } else {
            flags |= 1;
            struct ChildMix {
                mass: Vec<f32>,
                sums: RemovalSums,
                cfv: Vec<f32>,
            }
            let mixes: Vec<ChildMix> = children
                .iter()
                .map(|&(ai, c)| {
                    let mut mass = vec![0f32; N_COMBOS];
                    for (p, &(_, _, ix)) in oop_combos.iter().enumerate() {
                        mass[ix] = rho_oop[ix] * n.freqs[ai][p];
                    }
                    let sums = removal_sums(&mass, &oop_combos);
                    let cfv = (0..ip_combos.len())
                        .map(|q| {
                            (0..c.node.actions.len())
                                .map(|b| c.node.freqs[b][q] * c.node.evs[b][q])
                                .sum()
                        })
                        .collect();
                    ChildMix { mass, sums, cfv }
                })
                .collect();
            for (q, &(hi, lo, ix)) in ip_combos.iter().enumerate() {
                let mut mass_sum = 0f32;
                let mut acc = 0f32;
                for m in &mixes {
                    let w = m.sums.compat(&m.mass, hi, lo, ix);
                    mass_sum += w;
                    acc += w * m.cfv[q];
                }
                ip_cfv[ix] = if mass_sum > 0.0 {
                    acc / mass_sum
                } else {
                    stats.denom_zero += 1;
                    mixes.iter().map(|m| m.cfv[q]).sum::<f32>() / mixes.len() as f32
                };
            }
        }

        check_invariants(
            n,
            &rho_oop,
            &rho_ip,
            &oop_combos,
            &ip_combos,
            &oop_cfv,
            &ip_cfv,
            flags,
            children.len(),
            &mut stats,
        );

        let pot = n.pot_bb;
        for v in oop_cfv.iter_mut().chain(ip_cfv.iter_mut()) {
            *v /= pot;
        }
        let mut oop_equity = vec![0f32; N_COMBOS];
        if n.equity.len() == oop_combos.len() {
            for (p, &(_, _, ix)) in oop_combos.iter().enumerate() {
                oop_equity[ix] = n.equity[p];
            }
        }

        stats.roots += 1;
        out.push(RootRecord {
            board,
            pot_bb: pot,
            reach: root.reach,
            flags,
            oop_reach: rho_oop,
            ip_reach: rho_ip,
            oop_cfv_pot: oop_cfv,
            ip_cfv_pot: ip_cfv,
            oop_equity,
        });
    }
    (out, stats)
}

/// The two per-root validity checks (doc-comment at the top). Deviations
/// accumulate into `stats`; breaches fail the run at exit.
#[allow(clippy::too_many_arguments)]
fn check_invariants(
    n: &poker_trainer::tree::TreeNode,
    rho_oop: &[f32],
    rho_ip: &[f32],
    oop_combos: &[(u8, u8, usize)],
    ip_combos: &[(u8, u8, usize)],
    oop_cfv: &[f32],
    ip_cfv: &[f32],
    flags: u8,
    n_children: usize,
    stats: &mut FileStats,
) {
    // Entangled weights: own reach × unblocked opponent mass.
    let ip_sums = removal_sums(rho_ip, ip_combos);
    let oop_sums = removal_sums(rho_oop, oop_combos);
    let w_oop: Vec<f32> = oop_combos
        .iter()
        .map(|&(hi, lo, ix)| rho_oop[ix] * ip_sums.compat(rho_ip, hi, lo, ix))
        .collect();

    // Invariant 1: stored weights == w_oop up to one global scale (median
    // ratio), residues measured against the largest stored weight.
    let max_stored = n.weights.iter().cloned().fold(0f32, f32::max);
    if max_stored > 0.0 {
        let mut ratios: Vec<f32> = n
            .weights
            .iter()
            .zip(&w_oop)
            .filter(|&(&s, &w)| s > 1e-6 && w > 0.0)
            .map(|(&s, &w)| s / w)
            .collect();
        if !ratios.is_empty() {
            ratios.sort_by(f32::total_cmp);
            let r = ratios[ratios.len() / 2];
            let dev = n
                .weights
                .iter()
                .zip(&w_oop)
                .map(|(&s, &w)| (s - r * w).abs())
                .fold(0f32, f32::max)
                / max_stored;
            stats.max_weight_dev = stats.max_weight_dev.max(dev);
            if dev > WEIGHT_DEV_TOL {
                stats.weight_breaches += 1;
            }
        }
    }

    // Invariant 2: reach-weighted average cfvs of the two sides sum to the
    // pot (both sides share the same entangled-mass normalizer).
    if flags & 1 == 1 {
        let (mut s_o, mut e_o, mut s_i, mut e_i) = (0f64, 0f64, 0f64, 0f64);
        for (&(_, _, ix), &w) in oop_combos.iter().zip(&w_oop) {
            s_o += f64::from(w);
            e_o += f64::from(w) * f64::from(oop_cfv[ix]);
        }
        for &(hi, lo, ix) in ip_combos {
            let w = rho_ip[ix] * oop_sums.compat(rho_oop, hi, lo, ix);
            s_i += f64::from(w);
            e_i += f64::from(w) * f64::from(ip_cfv[ix]);
        }
        if s_o > 0.0 && s_i > 0.0 {
            let dev = (e_o / s_o + e_i / s_i - f64::from(n.pot_bb)).abs() as f32;
            if dev > stats.max_zero_sum_dev_bb {
                stats.max_zero_sum_dev_bb = dev;
                stats.worst_zero_sum = Some((n.line.join(" "), n_children, n.actions.len()));
            }
            if dev > ZERO_SUM_TOL_BB.max(0.003 * n.pot_bb) {
                stats.zero_sum_breaches += 1;
            }
        }
    }
}

// ---- shard writing ----------------------------------------------------------

fn write_record(
    w: &mut impl Write,
    flop_id: u16,
    formation_id: u8,
    r: &RootRecord,
) -> std::io::Result<()> {
    let mut buf = Vec::with_capacity(RECORD_BYTES);
    buf.extend_from_slice(&flop_id.to_le_bytes());
    buf.push(formation_id);
    buf.push(r.flags);
    buf.extend_from_slice(&r.board);
    buf.extend_from_slice(&r.pot_bb.to_le_bytes());
    buf.extend_from_slice(&r.reach.to_le_bytes());
    for block in [&r.oop_reach, &r.ip_reach, &r.oop_cfv_pot, &r.ip_cfv_pot] {
        for &x in block.iter() {
            buf.extend_from_slice(&f16_bits(x).to_le_bytes());
        }
    }
    debug_assert_eq!(buf.len(), RECORD_BYTES);
    w.write_all(&buf)
}

fn write_equity(w: &mut impl Write, r: &RootRecord) -> std::io::Result<()> {
    let mut buf = Vec::with_capacity(2 * N_COMBOS);
    for &x in &r.oop_equity {
        buf.extend_from_slice(&f16_bits(x).to_le_bytes());
    }
    w.write_all(&buf)
}

// ---- corpus.json ------------------------------------------------------------

#[derive(Serialize)]
struct Sidecar {
    version: u32,
    record_bytes: usize,
    n_combos: usize,
    card_id: &'static str,
    combo_index: &'static str,
    cfv_units: &'static str,
    mask: &'static str,
    val_rule: &'static str,
    flops: Vec<String>,
    formations: Vec<FormationSummary>,
}

#[derive(Serialize)]
struct ShardMeta {
    file: String,
    records: u64,
    flops: usize,
}

#[derive(Serialize)]
struct InvariantReport {
    max_weight_dev: f32,
    max_zero_sum_dev_bb: f32,
    weight_breaches: usize,
    zero_sum_breaches: usize,
    bad_nodes: usize,
    ip_masked: usize,
    denom_zero: usize,
}

impl InvariantReport {
    fn clean(&self) -> bool {
        self.weight_breaches == 0 && self.zero_sum_breaches == 0 && self.bad_nodes == 0
    }
}

#[derive(Serialize)]
struct FormationSummary {
    id: u8,
    name: String,
    dir: String,
    config_hash: String,
    pot_bb: f32,
    stack_bb: f32,
    files: usize,
    skipped_files: usize,
    roots: usize,
    train: ShardMeta,
    val: ShardMeta,
    val_equity_file: String,
    invariants: InvariantReport,
}

// ---- driver -----------------------------------------------------------------

#[derive(Default)]
struct FlopRegistry {
    ids: HashMap<String, u16>,
    list: Vec<String>,
}

impl FlopRegistry {
    fn id(&mut self, canon: &str) -> u16 {
        if let Some(&i) = self.ids.get(canon) {
            return i;
        }
        let i = self.list.len() as u16;
        self.list.push(canon.to_string());
        self.ids.insert(canon.to_string(), i);
        i
    }
}

fn run(args: &Args) -> Result<bool, String> {
    fs::create_dir_all(&args.out).map_err(|e| format!("{}: {e}", args.out.display()))?;
    let dir_names: Vec<String> = match &args.formations {
        Some(csv) => csv
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(String::from)
            .collect(),
        None => FORMATIONS
            .iter()
            .map(|f| f.id.to_string())
            .filter(|id| args.tables.join(id).is_dir())
            .collect(),
    };
    if dir_names.is_empty() {
        return Err(format!(
            "no formation dirs under {} — pass --formations",
            args.tables.display()
        ));
    }

    let mut registry = FlopRegistry::default();
    let mut summaries = Vec::new();
    for (i, name) in dir_names.iter().enumerate() {
        let fid = u8::try_from(i).map_err(|_| "more than 255 formations")?;
        summaries.push(extract_formation(args, name, fid, &mut registry)?);
    }

    for s in &summaries {
        println!(
            "{}: {} flops ({} skipped), {} roots → {} train + {} val records; \
             max weight dev {:.1e}, max zero-sum dev {:.4} bb — {}",
            s.dir,
            s.files,
            s.skipped_files,
            s.roots,
            s.train.records,
            s.val.records,
            s.invariants.max_weight_dev,
            s.invariants.max_zero_sum_dev_bb,
            if s.invariants.clean() { "OK" } else { "FAIL" },
        );
    }
    let clean = summaries.iter().all(|s| s.invariants.clean());
    let sidecar = Sidecar {
        version: 1,
        record_bytes: RECORD_BYTES,
        n_combos: N_COMBOS,
        card_id: "rank*4 + suit, suits cdhs: 2c=0 .. As=51",
        combo_index: "combo (hi,lo), hi>lo, at hi*(hi-1)/2 + lo",
        cfv_units: "pot (multiply by pot_bb for bb); fold = 0; mask = reach > 0",
        mask: "train on slots with reach > 0; flags bit0 = IP side present",
        val_rule: "fnv1a64(canonical flop) % 10 == 0",
        flops: registry.list,
        formations: summaries,
    };
    let path = args.out.join("corpus.json");
    fs::write(
        &path,
        serde_json::to_string_pretty(&sidecar).map_err(|e| e.to_string())?,
    )
    .map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(clean)
}

fn extract_formation(
    args: &Args,
    dir_name: &str,
    fid: u8,
    registry: &mut FlopRegistry,
) -> Result<FormationSummary, String> {
    let dir = args.tables.join(dir_name);
    let err = |e: &dyn std::fmt::Display| format!("{}: {e}", dir.display());

    // Exactly one header per formation dir (verified property of the store);
    // refusing to guess beats silently mixing configs.
    let mut headers = Vec::new();
    for entry in fs::read_dir(&dir).map_err(|e| err(&e))? {
        let name = entry.map_err(|e| err(&e))?.file_name();
        let name = name.to_string_lossy();
        if let Some(hash) = name
            .strip_prefix("header-")
            .and_then(|s| s.strip_suffix(".json"))
        {
            headers.push(hash.to_string());
        }
    }
    let [hash8] = headers.as_slice() else {
        return Err(err(&format!(
            "expected exactly one header-*.json, found {}",
            headers.len()
        )));
    };
    let header_path = dir.join(format!("header-{hash8}.json"));
    let header: TableHeader =
        serde_json::from_str(&fs::read_to_string(&header_path).map_err(|e| err(&e))?)
            .map_err(|e| err(&e))?;
    let base_oop = parse_weighted_range(&header.config.oop_range)?;
    let base_ip = parse_weighted_range(&header.config.ip_range)?;

    // Canonical flops only: legacy pre-iso-dedup files are exact duplicates
    // of their canonical representative.
    let mut flops: Vec<(String, String)> = Vec::new();
    let mut skipped = 0usize;
    for entry in fs::read_dir(&dir).map_err(|e| err(&e))? {
        let name = entry.map_err(|e| err(&e))?.file_name();
        let name = name.to_string_lossy();
        let Some(stem) = name.strip_suffix(".jsonl") else {
            continue;
        };
        let Some((flop, hash)) = stem.rsplit_once('-') else {
            continue;
        };
        if hash != hash8 {
            skipped += 1;
            continue;
        }
        match iso::canonical_flop(flop) {
            Some((canon, _)) if canon.eq_ignore_ascii_case(flop) => {
                flops.push((flop.to_string(), canon));
            }
            _ => skipped += 1,
        }
    }
    flops.sort();
    if let Some(l) = args.limit {
        flops.truncate(l);
    }

    let shard = |suffix: &str| args.out.join(format!("{dir_name}.{suffix}.bin"));
    let open = |suffix: &str| {
        fs::File::create(shard(suffix))
            .map(BufWriter::new)
            .map_err(|e| format!("{}: {e}", shard(suffix).display()))
    };
    let mut w_train = open("train")?;
    let mut w_val = open("val")?;
    let mut w_eq = open("val-equity")?;

    let mut agg = FileStats::default();
    let (mut n_train, mut n_val) = (0u64, 0u64);
    let (mut f_train, mut f_val) = (0usize, 0usize);
    for (i, (flop, canon)) in flops.iter().enumerate() {
        let table = PostflopTable::load(&dir, flop, hash8)
            .map_err(|e| format!("{dir_name}/{flop}: {e}"))?;
        let flop_id = registry.id(canon);
        let is_val = fnv1a64(canon).is_multiple_of(VAL_MOD);
        let (records, st) = extract_file(&table, &base_oop, &base_ip);
        agg.roots += st.roots;
        agg.ip_masked += st.ip_masked;
        agg.denom_zero += st.denom_zero;
        agg.bad_nodes += st.bad_nodes;
        agg.weight_breaches += st.weight_breaches;
        agg.zero_sum_breaches += st.zero_sum_breaches;
        agg.max_weight_dev = agg.max_weight_dev.max(st.max_weight_dev);
        if st.max_zero_sum_dev_bb > agg.max_zero_sum_dev_bb {
            agg.max_zero_sum_dev_bb = st.max_zero_sum_dev_bb;
            agg.worst_zero_sum = st
                .worst_zero_sum
                .map(|(l, c, a)| (format!("{flop}: {l}"), c, a));
        }
        let io = |e: std::io::Error| format!("{dir_name}/{flop}: {e}");
        if is_val {
            f_val += 1;
            for r in &records {
                write_record(&mut w_val, flop_id, fid, r).map_err(io)?;
                write_equity(&mut w_eq, r).map_err(io)?;
                n_val += 1;
            }
        } else {
            f_train += 1;
            for r in &records {
                write_record(&mut w_train, flop_id, fid, r).map_err(io)?;
                n_train += 1;
            }
        }
        if (i + 1) % 100 == 0 {
            eprintln!(
                "{dir_name}: {}/{} flops, {} roots",
                i + 1,
                flops.len(),
                agg.roots
            );
        }
    }
    for w in [&mut w_train, &mut w_val, &mut w_eq] {
        w.flush().map_err(|e| err(&e))?;
    }
    if let Some((line, stored, actions)) = &agg.worst_zero_sum {
        eprintln!(
            "{dir_name}: worst zero-sum root {line:?} ({stored}/{actions} children stored, \
             dev {:.4} bb)",
            agg.max_zero_sum_dev_bb
        );
    }

    Ok(FormationSummary {
        id: fid,
        name: header.formation,
        dir: dir_name.to_string(),
        config_hash: hash8.clone(),
        pot_bb: header.config.pot_bb,
        stack_bb: header.config.stack_bb,
        files: flops.len(),
        skipped_files: skipped,
        roots: agg.roots,
        train: ShardMeta {
            file: format!("{dir_name}.train.bin"),
            records: n_train,
            flops: f_train,
        },
        val: ShardMeta {
            file: format!("{dir_name}.val.bin"),
            records: n_val,
            flops: f_val,
        },
        val_equity_file: format!("{dir_name}.val-equity.bin"),
        invariants: InvariantReport {
            max_weight_dev: agg.max_weight_dev,
            max_zero_sum_dev_bb: agg.max_zero_sum_dev_bb,
            weight_breaches: agg.weight_breaches,
            zero_sum_breaches: agg.zero_sum_breaches,
            bad_nodes: agg.bad_nodes,
            ip_masked: agg.ip_masked,
            denom_zero: agg.denom_zero,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use poker_trainer::postflop_table::FORMAT_VERSION;
    use poker_trainer::solution::{GenInfo, SpotConfig};
    use poker_trainer::tree::TreeNode;

    // ---- unit math ----------------------------------------------------------

    #[test]
    fn combo_index_is_the_triangular_order() {
        assert_eq!(combo_index(1, 0), 0);
        assert_eq!(combo_index(2, 0), 1);
        assert_eq!(combo_index(2, 1), 2);
        assert_eq!(combo_index(51, 50), N_COMBOS - 1);
        assert_eq!(combo_of("AsAh"), Some((51, 50, N_COMBOS - 1)));
        assert_eq!(combo_of("2d2c"), Some((1, 0, 0)));
        // Solver order (high first) is not required by the parser.
        assert_eq!(combo_of("2c2d"), Some((1, 0, 0)));
        assert_eq!(combo_of("2c2c"), None);
        assert_eq!(combo_of("2c"), None);
    }

    #[test]
    fn f16_pins_known_bit_patterns_and_round_trips() {
        for (x, bits) in [
            (0.0f32, 0x0000u16),
            (1.0, 0x3c00),
            (-2.0, 0xc000),
            (0.5, 0x3800),
            (65504.0, 0x7bff),
            (1e9, 0x7c00),          // overflow → inf
            (5.9604645e-8, 0x0001), // min subnormal
            (-5.9604645e-8, 0x8001),
        ] {
            assert_eq!(f16_bits(x), bits, "{x}");
        }
        // Round-trip error bounded by half-ulp (2^-11 relative) on a sweep.
        let mut x = -20.0f32;
        while x < 20.0 {
            let back = f16_val(f16_bits(x));
            assert!(
                (back - x).abs() <= x.abs().max(0.062) * (2f32).powi(-11),
                "{x} -> {back}"
            );
            x += 0.017;
        }
    }

    #[test]
    fn fnv1a64_matches_reference_vectors() {
        assert_eq!(fnv1a64(""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv1a64("a"), 0xaf63_dc4c_8601_ec8c);
    }

    #[test]
    fn weighted_range_parses_classes_weights_and_real_headers() {
        let w = parse_weighted_range("AA:0.25, KK").unwrap();
        assert_eq!(w.iter().filter(|&&x| x > 0.0).count(), 12);
        assert_eq!(w[combo_of("AsAh").unwrap().2], 0.25);
        assert_eq!(w[combo_of("KsKh").unwrap().2], 1.0);
        let pairs = parse_weighted_range("22+").unwrap();
        assert_eq!(pairs.iter().filter(|&&x| x > 0.0).count(), 13 * 6);
        // A real curated header string parses wholesale.
        let real = parse_weighted_range(
            "22+,A2s+,K2s+,Q2s+,J4s+,T6s+,96s+,85s+,75s+,64s+,53s+,43s,\
             A2o+,K7o+,Q8o+,J8o+,T8o+,97o+,87o",
        )
        .unwrap();
        // 78 pair + ~228 suited + ~360 offsuit combos ≈ 666, all weight 1.
        assert!(real.iter().filter(|&&x| x > 0.0).count() > 600);
        assert!(real.iter().all(|&x| x == 0.0 || x == 1.0));
        assert!(parse_weighted_range("AA:zebra").is_err());
    }

    // ---- fixture ------------------------------------------------------------

    const FLOP: [&str; 3] = ["2c", "3c", "4c"];
    const TURN: &str = "5c";

    fn aa_kk_hands() -> Vec<String> {
        let mut v = Vec::new();
        for r in ["A", "K"] {
            for (i, a) in ["c", "d", "h", "s"].iter().enumerate() {
                for b in &["c", "d", "h", "s"][i + 1..] {
                    v.push(format!("{r}{b}{r}{a}")); // solver form: high id first
                }
            }
        }
        v
    }

    fn kk_hands() -> Vec<String> {
        aa_kk_hands().split_off(6)
    }

    /// Brute-force `own × unblocked opponent` weights — the O(n²) oracle the
    /// extractor's inclusion-exclusion must agree with.
    fn oracle_weights(
        own: &[String],
        own_rho: &[f32],
        opp: &[String],
        opp_rho: &[f32],
    ) -> Vec<f32> {
        own.iter()
            .zip(own_rho)
            .map(|(h, &r)| {
                let hc = [&h[..2], &h[2..]];
                let mass: f32 = opp
                    .iter()
                    .zip(opp_rho)
                    .filter(|(o, _)| !hc.iter().any(|c| o.contains(*c)))
                    .map(|(_, &m)| m)
                    .sum();
                r * mass
            })
            .collect()
    }

    struct Fixture {
        dir: std::path::PathBuf,
        hash: String,
        /// Per-position reach at the turn root, fixture-side.
        rho_oop: Vec<f32>,
        rho_ip: Vec<f32>,
        turn_freqs: Vec<Vec<f32>>,
        turn_evs: Vec<Vec<f32>>,
        child_freqs: [Vec<Vec<f32>>; 2],
        child_evs: [Vec<Vec<f32>>; 2],
    }

    /// Root (OOP, 12 hands AA+KK) → Check → IP (6 KK, weight 0.5) → Check →
    /// deal 5c → turn root (OOP). `complete` gives the root two actions with
    /// both IP children stored (exact rollback); otherwise a third action
    /// ("Bet 9.0bb") has no stored child — the IP-masking path.
    fn build_fixture(tag: &str, complete: bool) -> Fixture {
        let oop_hands = aa_kk_hands();
        let ip_hands = kk_hands();
        let base_oop = vec![1.0f32; 12];
        let base_ip = vec![0.5f32; 6];

        let spread = |n: usize, lo: f32, step: f32| -> Vec<f32> {
            (0..n).map(|i| lo + step * i as f32).collect()
        };
        let root_check: Vec<f32> = spread(12, 0.40, 0.02);
        let root_freqs = vec![
            root_check.clone(),
            root_check.iter().map(|c| 1.0 - c).collect(),
        ];
        let ip_check: Vec<f32> = spread(6, 0.55, 0.03);
        let ip_freqs = vec![ip_check.clone(), ip_check.iter().map(|c| 1.0 - c).collect()];

        let rho_oop: Vec<f32> = base_oop
            .iter()
            .zip(&root_check)
            .map(|(b, f)| b * f)
            .collect();
        let rho_ip: Vec<f32> = base_ip.iter().zip(&ip_check).map(|(b, f)| b * f).collect();

        // Turn root: distinct per-hand mixes and evs per action.
        let tr_a = spread(12, 0.20, 0.015);
        let tr_b = spread(12, 0.30, 0.010);
        let tr_last: Vec<f32> = tr_a
            .iter()
            .zip(&tr_b)
            .map(|(a, b)| if complete { 1.0 - a } else { 1.0 - a - b })
            .collect();
        let (turn_actions, turn_freqs, turn_evs) = if complete {
            (
                vec!["Check", "Bet 4.0bb"],
                vec![tr_a, tr_last],
                vec![spread(12, 1.0, 0.1), spread(12, 2.0, 0.05)],
            )
        } else {
            (
                vec!["Check", "Bet 4.0bb", "Bet 9.0bb"],
                vec![tr_a, tr_b, tr_last],
                vec![
                    spread(12, 1.0, 0.1),
                    spread(12, 2.0, 0.05),
                    spread(12, 0.5, 0.2),
                ],
            )
        };

        let c0_check = spread(6, 0.6, 0.02);
        let c0_freqs = vec![c0_check.clone(), c0_check.iter().map(|c| 1.0 - c).collect()];
        let c0_evs = vec![spread(6, 2.0, 0.2), spread(6, 3.0, 0.1)];
        let c1_check = spread(6, 0.3, 0.05);
        let c1_freqs = vec![c1_check.clone(), c1_check.iter().map(|c| 1.0 - c).collect()];
        let c1_evs = vec![spread(6, 1.0, 0.3), spread(6, 4.0, 0.05)];

        let flop_board: Vec<String> = FLOP.iter().map(|s| s.to_string()).collect();
        let mut turn_board = flop_board.clone();
        turn_board.push(TURN.into());
        let deal = format!("deal {TURN}");
        let node = |player: &str,
                    board: &Vec<String>,
                    line: Vec<&str>,
                    actions: Vec<&str>,
                    hands: &[String],
                    freqs: &[Vec<f32>],
                    evs: &[Vec<f32>],
                    weights: Vec<f32>,
                    reach: f32| {
            poker_trainer::postflop_table::TableNode {
                reach,
                node: TreeNode {
                    player: player.into(),
                    board: board.clone(),
                    pot_bb: 6.0,
                    line: line.into_iter().map(String::from).collect(),
                    actions: actions.into_iter().map(String::from).collect(),
                    dealable: vec![],
                    hands: hands.to_vec(),
                    freqs: freqs.to_vec(),
                    evs: evs.to_vec(),
                    weights,
                    equity: vec![0.5; hands.len()],
                },
            }
        };

        let nodes = [
            node(
                "oop",
                &flop_board,
                vec![],
                vec!["Check", "Bet 3.0bb"],
                &oop_hands,
                &root_freqs,
                &vec![vec![0.0; 12]; 2],
                oracle_weights(&oop_hands, &base_oop, &ip_hands, &base_ip),
                1.0,
            ),
            node(
                "ip",
                &flop_board,
                vec!["Check"],
                vec!["Check", "Bet 4.0bb"],
                &ip_hands,
                &ip_freqs,
                &vec![vec![0.0; 6]; 2],
                oracle_weights(&ip_hands, &base_ip, &oop_hands, &rho_oop),
                0.5,
            ),
            node(
                "oop",
                &turn_board,
                vec!["Check", "Check", &deal],
                turn_actions,
                &oop_hands,
                &turn_freqs,
                &turn_evs,
                oracle_weights(&oop_hands, &rho_oop, &ip_hands, &rho_ip),
                0.4,
            ),
            node(
                "ip",
                &turn_board,
                vec!["Check", "Check", &deal, "Check"],
                vec!["Check", "Bet 4.0bb"],
                &ip_hands,
                &c0_freqs,
                &c0_evs,
                vec![0.1; 6],
                0.3,
            ),
            node(
                "ip",
                &turn_board,
                vec!["Check", "Check", &deal, "Bet 4.0bb"],
                vec!["Fold", "Call"],
                &ip_hands,
                &c1_freqs,
                &c1_evs,
                vec![0.1; 6],
                0.2,
            ),
            // "Bet 9.0bb" child deliberately not stored (pruned).
        ];

        let config = SpotConfig {
            formation: "fixture".into(),
            oop_range: "AA,KK".into(),
            ip_range: "KK:0.5".into(),
            flop_sizes: "50%".into(),
            turn_sizes: "50%".into(),
            river_sizes: "50%".into(),
            stack_bb: 97.0,
            pot_bb: 6.0,
            rake_rate: 0.0,
            rake_cap_bb: 0.0,
        };
        let hash = config.hash8();
        let header = poker_trainer::postflop_table::TableHeader {
            version: FORMAT_VERSION,
            formation: "fixture".into(),
            config,
            config_hash: hash.clone(),
            generator: GenInfo {
                version: "test".into(),
                exploitability_bb: 0.0,
            },
            reach: 0.002,
        };
        let dir = std::env::temp_dir().join(format!("evc-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join(format!("header-{hash}.json")),
            serde_json::to_string(&header).unwrap(),
        )
        .unwrap();
        let lines: Vec<String> = nodes
            .iter()
            .map(|n| serde_json::to_string(n).unwrap())
            .collect();
        fs::write(dir.join(format!("2c3c4c-{hash}.jsonl")), lines.join("\n")).unwrap();
        Fixture {
            dir,
            hash,
            rho_oop,
            rho_ip,
            turn_freqs,
            turn_evs,
            child_freqs: [c0_freqs, c1_freqs],
            child_evs: [c0_evs, c1_evs],
        }
    }

    fn load_fixture(fx: &Fixture) -> PostflopTable {
        PostflopTable::load(&fx.dir, "2c3c4c", &fx.hash).unwrap()
    }

    #[test]
    fn extract_file_reconstructs_reaches_and_cfvs() {
        let fx = build_fixture("main", true);
        let table = load_fixture(&fx);
        let base_oop = parse_weighted_range("AA,KK").unwrap();
        let base_ip = parse_weighted_range("KK:0.5").unwrap();
        let (records, stats) = extract_file(&table, &base_oop, &base_ip);
        fs::remove_dir_all(&fx.dir).unwrap();

        assert_eq!(stats.roots, 1);
        assert_eq!(stats.bad_nodes, 0);
        assert_eq!(stats.ip_masked, 0);
        assert_eq!(stats.denom_zero, 0);
        assert_eq!(
            stats.weight_breaches, 0,
            "oracle weights must satisfy invariant 1 (max dev {})",
            stats.max_weight_dev
        );
        let r = &records[0];
        assert_eq!(r.flags, 1);
        assert_eq!(r.board, [0, 4, 8, 12]);
        assert_eq!(r.pot_bb, 6.0);
        assert!((r.reach - 0.4).abs() < 1e-6);

        let oop_hands = aa_kk_hands();
        let ip_hands = kk_hands();
        // Reach vectors match the fixture-side products exactly.
        for (p, h) in oop_hands.iter().enumerate() {
            let ix = combo_of(h).unwrap().2;
            assert!((r.oop_reach[ix] - fx.rho_oop[p]).abs() < 1e-6, "{h}");
        }
        for (q, h) in ip_hands.iter().enumerate() {
            let ix = combo_of(h).unwrap().2;
            assert!((r.ip_reach[ix] - fx.rho_ip[q]).abs() < 1e-6, "{h}");
        }
        // Combos outside the ranges (or board-blocked) stay zero.
        assert_eq!(r.oop_reach[combo_of("QsQh").unwrap().2], 0.0);
        assert_eq!(r.ip_reach[combo_of("AsAh").unwrap().2], 0.0);

        // OOP cfv: Σ_a freq·ev of the stored mix, in pot units.
        for (p, h) in oop_hands.iter().enumerate() {
            let want: f32 = (0..fx.turn_freqs.len())
                .map(|a| fx.turn_freqs[a][p] * fx.turn_evs[a][p])
                .sum();
            let ix = combo_of(h).unwrap().2;
            assert!(
                (r.oop_cfv_pot[ix] - want / 6.0).abs() < 1e-6,
                "{h}: {} vs {}",
                r.oop_cfv_pot[ix],
                want / 6.0
            );
        }

        // IP cfv: mix the two stored children by the unblocked OOP mass
        // taking each action — checked against a brute-force O(n²)
        // reimplementation of the card-removal weighting.
        for (q, h) in ip_hands.iter().enumerate() {
            let hc = [&h[..2], &h[2..]];
            let (mut num, mut den) = (0f32, 0f32);
            for (ci, ai) in [(0usize, 0usize), (1, 1)] {
                let mass: f32 = oop_hands
                    .iter()
                    .enumerate()
                    .filter(|(_, o)| !hc.iter().any(|c| o.contains(*c)))
                    .map(|(p, _)| fx.rho_oop[p] * fx.turn_freqs[ai][p])
                    .sum();
                let cfv: f32 = (0..2)
                    .map(|b| fx.child_freqs[ci][b][q] * fx.child_evs[ci][b][q])
                    .sum();
                num += mass * cfv;
                den += mass;
            }
            let ix = combo_of(h).unwrap().2;
            assert!(
                (r.ip_cfv_pot[ix] - num / den / 6.0).abs() < 1e-5,
                "{h}: {} vs {}",
                r.ip_cfv_pot[ix],
                num / den / 6.0
            );
        }
    }

    #[test]
    fn incomplete_children_mask_the_ip_side() {
        let fx = build_fixture("partial", false);
        let table = load_fixture(&fx);
        let base_oop = parse_weighted_range("AA,KK").unwrap();
        let base_ip = parse_weighted_range("KK:0.5").unwrap();
        let (records, stats) = extract_file(&table, &base_oop, &base_ip);
        fs::remove_dir_all(&fx.dir).unwrap();

        assert_eq!(stats.roots, 1);
        assert_eq!(stats.ip_masked, 1);
        assert_eq!(
            stats.max_zero_sum_dev_bb, 0.0,
            "zero-sum runs only on complete roots"
        );
        let r = &records[0];
        assert_eq!(r.flags, 0, "IP side masked");
        assert!(r.ip_cfv_pot.iter().all(|&x| x == 0.0));
        // The OOP side and both reach vectors stay fully usable.
        assert!(r.oop_cfv_pot.iter().any(|&x| x != 0.0));
        assert!(r.ip_reach.iter().any(|&x| x != 0.0));
    }

    #[test]
    fn record_bytes_round_trip_the_layout() {
        let fx = build_fixture("layout", true);
        let table = load_fixture(&fx);
        let base_oop = parse_weighted_range("AA,KK").unwrap();
        let base_ip = parse_weighted_range("KK:0.5").unwrap();
        let (records, _) = extract_file(&table, &base_oop, &base_ip);
        fs::remove_dir_all(&fx.dir).unwrap();

        let r = &records[0];
        let mut buf = Vec::new();
        write_record(&mut buf, 1754, 3, r).unwrap();
        assert_eq!(buf.len(), RECORD_BYTES);
        assert_eq!(u16::from_le_bytes([buf[0], buf[1]]), 1754);
        assert_eq!(buf[2], 3);
        assert_eq!(buf[3], r.flags);
        assert_eq!(&buf[4..8], &r.board);
        assert_eq!(f32::from_le_bytes(buf[8..12].try_into().unwrap()), 6.0);
        assert_eq!(f32::from_le_bytes(buf[12..16].try_into().unwrap()), 0.4);
        // Spot-check one slot in each f16 block against the f32 source.
        let ix = combo_of("KsKh").unwrap().2;
        for (block, src) in [
            (0usize, &r.oop_reach),
            (1, &r.ip_reach),
            (2, &r.oop_cfv_pot),
            (3, &r.ip_cfv_pot),
        ] {
            let off = 16 + 2 * (block * N_COMBOS + ix);
            let got = f16_val(u16::from_le_bytes([buf[off], buf[off + 1]]));
            assert!(
                (got - src[ix]).abs() <= src[ix].abs().max(0.062) * (2f32).powi(-11),
                "block {block}: {got} vs {}",
                src[ix]
            );
        }
    }
}
