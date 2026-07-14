//! `poker-trainer table` — browse a solved spot's whole strategy as a
//! GTO-Wizard-style 13×13 starting-hand grid, each cell colored by its
//! equilibrium action mix.
//!
//! The data is already in [`SolvedSpot`]: one
//! [`NodeStrategy`](crate::solution::NodeStrategy) per combo. This
//! module folds those ~1326 combos into the 169 canonical cells and draws them.
//! The folding + coloring are pure (and unit-tested below); the TUI half just
//! renders the grid and walks the cursor / cycles nodes.

use crate::eval::{classify_hand, Bucket};
use crate::report::is_aggressive;
use crate::solution::SolvedSpot;
use crate::trainer::{fmt_hand_str, parse_hole};
use crate::tree::{RunoutSummary, TreeNode, TreeWalk};
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind};
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;
use rs_poker::core::Card;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};

/// Ranks high→low; a card's index into this is its grid row/col (A = 0).
const RANKS: &[u8] = b"AKQJT98765432";
/// Inner width of a grid cell, in chars (fits a 3-char label + color margins).
const CELL_W: usize = 5;

/// One combo's detail inside a cell, kept so the side panel and the bucket
/// filter can stop averaging. Snapshot sources carry no reach/equity, so those
/// default to `1.0` / `NaN`.
#[derive(Debug, Clone)]
pub struct ComboRow {
    /// "AhKh".
    pub hand: String,
    /// Reach weight at this node (combo mass), `1.0` when the source has none.
    pub weight: f32,
    /// Equity vs. the villain's reaching range, `NaN` when the source has none.
    pub equity: f32,
    /// Best-action EV in bb (`0.0` when the source carries no EVs).
    pub ev: f32,
    /// Frequency per action, parallel to the cell's `actions`.
    pub freqs: Vec<f32>,
    /// Flop made-hand bucket (`None` if the board/hole didn't parse).
    pub bucket: Option<Bucket>,
}

/// One aggregated grid cell: a canonical hand and its combo-averaged mix.
/// Aggregates are reach-weighted means; with unit weights (snapshots) that's
/// the plain mean, so the v1 grid renders identically.
#[derive(Debug, Clone)]
pub struct Cell {
    /// "AA" / "AKs" / "AKo".
    pub label: String,
    /// How many present combos folded into this cell (blocked ones are absent).
    pub combos: u32,
    /// Action labels, taken from the first combo (a node shares one action set).
    pub actions: Vec<String>,
    /// Mean frequency per action, parallel to `actions`.
    pub freqs: Vec<f32>,
    /// Mean reach weight over combos, in `[0, 1]`.
    pub weight: f32,
    /// Reach-weighted mean equity (`NaN` for snapshot sources).
    pub equity: f32,
    /// Reach-weighted mean best-action EV, bb.
    pub ev: f32,
    /// Per-combo rows, for the side panel and the bucket filter.
    pub rows: Vec<ComboRow>,
}

/// The 13×13 canonical grid.
pub type Grid = [[Option<Cell>; 13]; 13];

/// Index of a rank byte into [`RANKS`] (A=0 … 2=12); `None` if not a rank.
fn rank_idx(rank: u8) -> Option<usize> {
    RANKS.iter().position(|&r| r == rank)
}

/// Canonical label + `(row, col)` for a two-card holding (`None` if a rank
/// doesn't parse). Standard chart layout: pairs on the diagonal, suited in the
/// upper-right triangle, offsuit in the lower-left.
pub fn canonical(hole: &[Card; 2]) -> Option<(String, usize, usize)> {
    let a = rank_idx(char::from(hole[0].value) as u8)?;
    let b = rank_idx(char::from(hole[1].value) as u8)?;
    let (hi, lo) = if a <= b { (a, b) } else { (b, a) }; // hi = smaller index = higher rank
    let (hi_c, lo_c) = (RANKS[hi] as char, RANKS[lo] as char);
    if hi == lo {
        Some((format!("{hi_c}{lo_c}"), hi, lo)) // pair → diagonal
    } else if hole[0].suit == hole[1].suit {
        Some((format!("{hi_c}{lo_c}s"), hi, lo)) // suited → upper-right
    } else {
        Some((format!("{hi_c}{lo_c}o"), lo, hi)) // offsuit → lower-left
    }
}

/// The first three board cards as a flop, for bucket classification.
/// ponytail: buckets use the flop even on turn/river nodes — same convention
/// as the hand drill and stats; classify on the full board if it ever misleads.
fn flop_cards(board: &[String]) -> Option<[Card; 3]> {
    match board {
        [a, b, c, ..] => Some([
            Card::try_from(a.as_str()).ok()?,
            Card::try_from(b.as_str()).ok()?,
            Card::try_from(c.as_str()).ok()?,
        ]),
        _ => None,
    }
}

/// Add one combo's strategy to its canonical cell.
#[allow(clippy::too_many_arguments)] // a plain fold step; a params struct would just rename these
fn push_combo(
    grid: &mut Grid,
    hand: &str,
    actions: &[String],
    freqs: &[f32],
    evs: &[f32],
    weight: f32,
    equity: f32,
    flop: Option<[Card; 3]>,
) {
    let Some(hole) = parse_hole(hand) else { return };
    let Some((label, r, c)) = canonical(&hole) else {
        return;
    };
    let cell = grid[r][c].get_or_insert_with(|| Cell {
        label,
        combos: 0,
        actions: actions.to_vec(),
        freqs: vec![],
        weight: 0.0,
        equity: f32::NAN,
        ev: 0.0,
        rows: vec![],
    });
    // ponytail: a node shares one action set, so this guard never trips in
    // practice — it just keeps a mismatched combo from panicking the zip.
    if cell.actions.len() != freqs.len() {
        return;
    }
    cell.rows.push(ComboRow {
        hand: hand.to_string(),
        weight,
        equity,
        ev: evs.iter().copied().fold(f32::NAN, f32::max), // best action; NaN if the source has no EVs
        freqs: freqs.to_vec(),
        bucket: flop.map(|f| classify_hand(hole, f)),
    });
}

/// Compute each cell's aggregates from its rows: reach-weighted means, falling
/// back to plain means for zero-reach cells (so the strategy bar still shows).
/// Weights are rescaled so the most-reaching combo is `1.0` — the solver's raw
/// scale is combo-count-like, not `[0, 1]`.
fn finalize(grid: &mut Grid) {
    let wmax = grid
        .iter()
        .flatten()
        .flatten()
        .flat_map(|cell| cell.rows.iter().map(|r| r.weight))
        .fold(0.0_f32, f32::max);
    if wmax > 0.0 {
        for cell in grid.iter_mut().flatten().flatten() {
            for r in &mut cell.rows {
                r.weight /= wmax;
            }
        }
    }
    for cell in grid.iter_mut().flatten().flatten() {
        cell.combos = cell.rows.len() as u32;
        let wsum: f32 = cell.rows.iter().map(|r| r.weight).sum();
        let weighted = wsum > 1e-9;
        let w = |r: &ComboRow| if weighted { r.weight } else { 1.0 };
        let div = if weighted {
            wsum
        } else {
            cell.combos.max(1) as f32
        };
        cell.freqs = vec![0.0; cell.rows.first().map_or(0, |r| r.freqs.len())];
        for r in &cell.rows {
            for (acc, f) in cell.freqs.iter_mut().zip(&r.freqs) {
                *acc += w(r) * f;
            }
        }
        for f in &mut cell.freqs {
            *f /= div;
        }
        cell.weight = wsum / cell.combos.max(1) as f32;
        cell.equity = cell.rows.iter().map(|r| w(r) * r.equity).sum::<f32>() / div;
        cell.ev = cell.rows.iter().map(|r| w(r) * r.ev).sum::<f32>() / div;
    }
}

/// Fold a spot's per-combo strategies into the 13×13 canonical grid, averaging
/// each cell's action frequencies over the combos that land in it.
pub fn build_grid(spot: &SolvedSpot) -> Grid {
    let mut grid: Grid = std::array::from_fn(|_| std::array::from_fn(|_| None));
    let flop = flop_cards(&spot.board);
    for hs in &spot.strategies {
        let ns = &hs.strategy;
        push_combo(
            &mut grid,
            &hs.hand,
            &ns.actions,
            &ns.frequencies,
            &ns.action_ev,
            1.0,
            f32::NAN,
            flop,
        );
    }
    finalize(&mut grid);
    grid
}

/// Fold a tree node into the grid, keeping its reach weights and equity.
pub fn build_grid_node(node: &TreeNode) -> Grid {
    let mut grid: Grid = std::array::from_fn(|_| std::array::from_fn(|_| None));
    let flop = flop_cards(&node.board);
    for (j, hand) in node.hands.iter().enumerate() {
        let freqs: Vec<f32> = node.freqs.iter().map(|per| per[j]).collect();
        let evs: Vec<f32> = node.evs.iter().map(|per| per[j]).collect();
        push_combo(
            &mut grid,
            hand,
            &node.actions,
            &freqs,
            &evs,
            node.weights.get(j).copied().unwrap_or(1.0),
            node.equity.get(j).copied().unwrap_or(f32::NAN),
            flop,
        );
    }
    finalize(&mut grid);
    grid
}

/// Per-cell locked action frequencies, keyed by grid `(row, col)` (P10).
pub type CellLocks = HashMap<(usize, usize), Vec<f32>>;

/// Expand cell-granular locks to the node's full `[action][hand]` strategy for
/// the `lock` op: every combo in a locked cell gets that cell's frequencies,
/// and every other hand stays all-`0.0` (left free for the re-solve).
pub fn expand_lock(node: &TreeNode, locks: &CellLocks) -> Vec<Vec<f32>> {
    let n = node.actions.len();
    let mut strat = vec![vec![0.0; node.hands.len()]; n];
    for (j, hand) in node.hands.iter().enumerate() {
        let Some((_, r, c)) = parse_hole(hand).as_ref().and_then(canonical) else {
            continue;
        };
        if let Some(freqs) = locks.get(&(r, c)) {
            for (a, f) in freqs.iter().enumerate().take(n) {
                strat[a][j] = *f;
            }
        }
    }
    strat
}

/// Snapshot each cell's best-action EV, for the EV-delta lens after a re-solve.
fn baseline_map(grid: &Grid) -> HashMap<(usize, usize), f32> {
    let mut m = HashMap::new();
    for (r, row) in grid.iter().enumerate() {
        for (c, cell) in row.iter().enumerate() {
            if let Some(cell) = cell {
                m.insert((r, c), cell.ev);
            }
        }
    }
    m
}

/// Linear RGB blend from `a` to `b` at `u` in `[0, 1]`, as a terminal color.
fn lerp(a: [f32; 3], b: [f32; 3], u: f32) -> Color {
    Color::Rgb(
        (a[0] + (b[0] - a[0]) * u) as u8,
        (a[1] + (b[1] - a[1]) * u) as u8,
        (a[2] + (b[2] - a[2]) * u) as u8,
    )
}

/// EV-delta color: green as EV rises, red as it falls, gray near zero, scaled
/// by the grid's largest absolute change so the ramp always spans the data.
fn delta_color(d: f32, scale: f32) -> Color {
    let m = if scale > 1e-9 {
        (d.abs() / scale).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let gray = [70.0, 70.0, 70.0];
    let end = if d >= 0.0 {
        [60.0, 165.0, 90.0]
    } else {
        [200.0, 60.0, 50.0]
    };
    lerp(gray, end, m)
}

/// Which per-cell reduction the grid shows, toggled by key (design doc 03):
/// `s` strategy, `w` range mass, `e` EV, `y` equity, `d` EV-delta (P10, vs. the
/// pre-resolve baseline).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lens {
    Strategy,
    Range,
    Ev,
    Equity,
    Delta,
}

/// Low→high color ramp for the scalar lenses: red → yellow → green, reusing
/// the app's bet-red and call-green endpoints.
fn heat_color(t: f32) -> Color {
    let t = t.clamp(0.0, 1.0);
    let (red, yellow, green) = (
        [200.0, 60.0, 50.0],
        [220.0, 190.0, 60.0],
        [60.0, 165.0, 90.0],
    );
    if t < 0.5 {
        lerp(red, yellow, t * 2.0)
    } else {
        lerp(yellow, green, (t - 0.5) * 2.0)
    }
}

/// `(min, max)` of finite cell EVs, for normalizing the EV lens.
fn ev_range(grid: &Grid) -> (f32, f32) {
    let mut lo = f32::INFINITY;
    let mut hi = f32::NEG_INFINITY;
    for cell in grid.iter().flatten().flatten() {
        if cell.ev.is_finite() {
            lo = lo.min(cell.ev);
            hi = hi.max(cell.ev);
        }
    }
    if lo > hi {
        (0.0, 1.0)
    } else {
        (lo, hi)
    }
}

/// Fraction of a cell's combos in `bucket` (the `f` filter dims cells < ½).
fn bucket_frac(cell: &Cell, bucket: Bucket) -> f32 {
    if cell.rows.is_empty() {
        return 0.0;
    }
    let hits = cell
        .rows
        .iter()
        .filter(|r| r.bucket == Some(bucket))
        .count();
    hits as f32 / cell.rows.len() as f32
}

/// GTO-Wizard-ish color for an action: fold blue, check/call green, bet/raise a
/// red ramp that deepens with bet size (`idx`/`n` order the sizes within a node).
pub fn action_color(label: &str, idx: usize, n: usize) -> Color {
    match label
        .split_whitespace()
        .next()
        .unwrap_or("")
        .to_ascii_lowercase()
        .as_str()
    {
        "fold" => Color::Rgb(60, 110, 200),
        "check" | "call" => Color::Rgb(60, 165, 90),
        _ => {
            // bet / raise / all-in: orange (small) → deep red (large).
            let t = if n > 1 {
                idx as f32 / (n - 1) as f32
            } else {
                1.0
            };
            Color::Rgb(230, 30 + (150.0 * (1.0 - t)) as u8, 40)
        }
    }
}

/// Split a cell's frequencies into integer char widths summing to `w`
/// (largest-remainder, so the bar fills exactly the cell).
fn segment_widths(freqs: &[f32], w: usize) -> Vec<usize> {
    if freqs.is_empty() {
        return vec![];
    }
    let raw: Vec<f32> = freqs.iter().map(|f| f * w as f32).collect();
    let mut widths: Vec<usize> = raw.iter().map(|x| x.floor() as usize).collect();
    let mut used: usize = widths.iter().sum();
    // hand out the leftover to the largest fractional remainders, one each.
    let mut order: Vec<usize> = (0..freqs.len()).collect();
    order.sort_by(|&a, &b| (raw[b] - widths[b] as f32).total_cmp(&(raw[a] - widths[a] as f32)));
    let mut k = 0;
    while used < w {
        widths[order[k % order.len()]] += 1;
        used += 1;
        k += 1;
    }
    widths
}

/// A scalar lens's `(background, text)` colors for one cell; `None` means the
/// strategy bar draws instead.
fn scalar_colors(
    cell: &Cell,
    lens: Lens,
    ev_lo_hi: (f32, f32),
    delta: Option<f32>,
    delta_scale: f32,
) -> Option<(Color, Color)> {
    let dark = (Color::Rgb(30, 30, 30), Color::DarkGray);
    let t = match lens {
        Lens::Strategy => return None,
        Lens::Delta => {
            return Some(match delta.filter(|d| d.is_finite()) {
                Some(d) => (delta_color(d, delta_scale), Color::Black),
                None => dark,
            })
        }
        Lens::Range => {
            let t = cell.weight.clamp(0.0, 1.0);
            let v = (25.0 + 205.0 * t) as u8;
            let fg = if t > 0.55 { Color::Black } else { Color::White };
            return Some((Color::Rgb(v, v, v), fg));
        }
        Lens::Ev => {
            if !cell.ev.is_finite() {
                return Some(dark);
            }
            let (lo, hi) = ev_lo_hi;
            if hi > lo {
                (cell.ev - lo) / (hi - lo)
            } else {
                0.5
            }
        }
        Lens::Equity => {
            if !cell.equity.is_finite() {
                return Some(dark);
            }
            cell.equity
        }
    };
    Some((heat_color(t), Color::Black))
}

/// Styled spans for one cell: the lens's coloring (strategy bar or scalar
/// heat) with the hand label centered on top; `dimmed` = filtered out.
fn cell_spans(
    cell: &Cell,
    focused: bool,
    lens: Lens,
    ev_lo_hi: (f32, f32),
    dimmed: bool,
    delta: Option<f32>,
    delta_scale: f32,
) -> Vec<Span<'static>> {
    let mut bg = [Color::Rgb(40, 40, 40); CELL_W]; // unfilled remainder, dark
    let mut fg = Color::White;
    if dimmed {
        bg = [Color::Rgb(25, 25, 25); CELL_W];
        fg = Color::DarkGray;
    } else if let Some((color, text)) = scalar_colors(cell, lens, ev_lo_hi, delta, delta_scale) {
        bg = [color; CELL_W];
        fg = text;
    } else {
        let widths = segment_widths(&cell.freqs, CELL_W);
        let mut pos = 0;
        // draw strongest action first (leftmost); fold ends up rightmost.
        for i in (0..widths.len()).rev() {
            let col = action_color(&cell.actions[i], i, cell.actions.len());
            for _ in 0..widths[i] {
                if pos < CELL_W {
                    bg[pos] = col;
                    pos += 1;
                }
            }
        }
    }
    let label: Vec<char> = cell.label.chars().collect();
    let start = CELL_W.saturating_sub(label.len()) / 2;
    (0..CELL_W)
        .map(|p| {
            let ch = label.get(p.wrapping_sub(start)).copied().unwrap_or(' ');
            let ch = if p >= start { ch } else { ' ' };
            let mut style = Style::default().bg(bg[p]).fg(fg);
            if focused {
                style = style.add_modifier(Modifier::BOLD | Modifier::UNDERLINED);
            }
            Span::styled(ch.to_string(), style)
        })
        .collect()
}

/// The grid as styled lines: a rank header row, then 13 rows of 13 cells.
fn grid_lines(
    grid: &Grid,
    cursor: (usize, usize),
    lens: Lens,
    filter: Option<Bucket>,
    baseline: Option<&HashMap<(usize, usize), f32>>,
) -> Vec<Line<'static>> {
    let dim = Style::default().fg(Color::DarkGray);
    let ev_lo_hi = ev_range(grid);
    // For the delta lens: each cell's EV change and the grid's largest |change|.
    let cell_delta = |r: usize, c: usize, cell: &Cell| {
        baseline.and_then(|b| b.get(&(r, c))).map(|&e| cell.ev - e)
    };
    let delta_scale = match (lens, baseline) {
        (Lens::Delta, Some(_)) => grid
            .iter()
            .enumerate()
            .flat_map(|(r, row)| {
                row.iter().enumerate().filter_map(move |(c, cell)| {
                    cell.as_ref().and_then(|cell| cell_delta(r, c, cell))
                })
            })
            .fold(0.0_f32, |m, d| m.max(d.abs())),
        _ => 1.0,
    };
    let mut lines = Vec::with_capacity(14);

    let mut header = vec![Span::raw("   ")]; // gutter under the row-rank column
    for &r in RANKS {
        header.push(Span::styled(format!("{:^CELL_W$}", r as char), dim));
    }
    lines.push(Line::from(header));

    for (r, row) in grid.iter().enumerate() {
        let mut spans = vec![Span::styled(format!("{:>2} ", RANKS[r] as char), dim)];
        for (c, cell) in row.iter().enumerate() {
            match cell {
                Some(cell) => {
                    let dimmed = filter.is_some_and(|b| bucket_frac(cell, b) < 0.5);
                    let delta = cell_delta(r, c, cell);
                    spans.extend(cell_spans(
                        cell,
                        cursor == (r, c),
                        lens,
                        ev_lo_hi,
                        dimmed,
                        delta,
                        delta_scale,
                    ));
                }
                None => spans.push(Span::raw(" ".repeat(CELL_W))),
            }
        }
        lines.push(Line::from(spans));
    }
    lines
}

/// A `w`-wide bar of colored blocks split by action frequency.
fn freq_bar(actions: &[String], freqs: &[f32], w: usize) -> Vec<Span<'static>> {
    // strongest action first (leftmost) to match the cell bars.
    segment_widths(freqs, w)
        .iter()
        .enumerate()
        .rev()
        .filter(|(_, &n)| n > 0)
        .map(|(i, &n)| {
            Span::styled(
                "█".repeat(n),
                Style::default().fg(action_color(&actions[i], i, actions.len())),
            )
        })
        .collect()
}

/// The detail panel for the focused cell: hand, combo count, exact mix — and,
/// when the source carries reach/equity (tree mode), the cell aggregates plus
/// one row per suit combo instead of the average.
fn detail_lines(
    grid: &Grid,
    cursor: (usize, usize),
    locks: Option<&CellLocks>,
    blockers: Option<&Blockers>,
) -> Vec<Line<'static>> {
    let Some(cell) = grid[cursor.0][cursor.1].as_ref() else {
        return vec![Line::from("(no combos on this board)")];
    };
    let mut lines = vec![Line::from(vec![
        Span::styled(
            cell.label.clone(),
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Span::raw(format!(
            "   {} combo{}",
            cell.combos,
            if cell.combos == 1 { "" } else { "s" }
        )),
    ])];
    if let Some(freqs) = locks.and_then(|l| l.get(&cursor)) {
        // Named the dominant locked action (the UI sets pure locks).
        if let Some((i, f)) = freqs.iter().enumerate().max_by(|a, b| a.1.total_cmp(b.1)) {
            lines.push(Line::from(Span::styled(
                format!(
                    "LOCK → {} {:.0}%",
                    cell.actions.get(i).cloned().unwrap_or_default(),
                    f * 100.0
                ),
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            )));
        }
    }
    if cell.equity.is_finite() {
        lines.push(Line::from(format!(
            "reach {:>3.0}%   eq {:>3.0}%   ev {:+.2}bb",
            cell.weight * 100.0,
            cell.equity * 100.0,
            cell.ev
        )));
    }
    // P8 blocker column: mean over the cell's combos of the villain continue
    // mass each blocks (vs. this node's biggest bet).
    let row_blocked = |row: &ComboRow| Some(blockers?.blocked(parse_hole(&row.hand)?));
    if let Some(b) = blockers {
        let blocked: Vec<f32> = cell.rows.iter().filter_map(row_blocked).collect();
        if !blocked.is_empty() {
            lines.push(Line::from(format!(
                "blocks {:>2.0}% of villain's continues vs {}",
                blocked.iter().sum::<f32>() / blocked.len() as f32 * 100.0,
                b.action
            )));
        }
    }
    for (i, action) in cell.actions.iter().enumerate() {
        lines.push(Line::from(vec![
            Span::styled(
                "██ ",
                Style::default().fg(action_color(action, i, cell.actions.len())),
            ),
            Span::raw(format!("{:<12} {:>5.1}%", action, cell.freqs[i] * 100.0)),
        ]));
    }
    if cell.rows.first().is_some_and(|r| r.equity.is_finite()) {
        lines.push(Line::default());
        for row in &cell.rows {
            let mut spans = freq_bar(&cell.actions, &row.freqs, CELL_W);
            spans.push(Span::raw(format!(
                " {}  w {:>3.0}%  eq {:>3.0}%  ev {:+.2}",
                fmt_hand_str(&row.hand),
                row.weight * 100.0,
                row.equity * 100.0,
                row.ev
            )));
            if let Some(blk) = row_blocked(row) {
                spans.push(Span::raw(format!("  blk {:>2.0}%", blk * 100.0)));
            }
            lines.push(Line::from(spans));
        }
    }
    lines
}

/// An action→color legend for `actions`.
fn action_legend(actions: &[String]) -> Vec<Line<'static>> {
    // strongest action first, to match the reversed cell bars.
    actions
        .iter()
        .enumerate()
        .rev()
        .map(|(i, action)| {
            Line::from(vec![
                Span::styled(
                    "██ ",
                    Style::default().fg(action_color(action, i, actions.len())),
                ),
                Span::raw(action.clone()),
            ])
        })
        .collect()
}

/// The action→color legend, read off any present cell (a node shares one set).
fn legend_lines(grid: &Grid) -> Vec<Line<'static>> {
    let Some(cell) = grid.iter().flatten().flatten().next() else {
        return vec![Line::from("(no data)")];
    };
    action_legend(&cell.actions)
}

fn draw(
    f: &mut Frame,
    spot: &SolvedSpot,
    grid: &Grid,
    cursor: (usize, usize),
    node: (usize, usize),
) {
    let rows = Layout::vertical([
        Constraint::Length(4),  // header
        Constraint::Length(16), // grid (1 rank row + 13 + borders)
        Constraint::Min(5),     // detail | legend
        Constraint::Length(1),  // help
    ])
    .split(f.area());

    let header = Paragraph::new(vec![
        Line::from(Span::styled(
            spot.label.clone(),
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from(format!(
            "Board {}   Pot {:.1}bb   {}",
            fmt_hand_str(&spot.board.join("")),
            spot.pot_bb,
            spot.villain_action
        )),
    ])
    .block(Block::default().borders(Borders::ALL).title(format!(
        " node {}/{} ",
        node.0 + 1,
        node.1
    )));
    f.render_widget(header, rows[0]);

    let grid_widget = Paragraph::new(grid_lines(grid, cursor, Lens::Strategy, None, None))
        .block(Block::default().borders(Borders::ALL).title(" strategy "));
    f.render_widget(grid_widget, rows[1]);

    let mid =
        Layout::horizontal([Constraint::Percentage(55), Constraint::Percentage(45)]).split(rows[2]);
    f.render_widget(
        Paragraph::new(detail_lines(grid, cursor, None, None))
            .block(Block::default().borders(Borders::ALL).title(" hand ")),
        mid[0],
    );
    f.render_widget(
        Paragraph::new(legend_lines(grid))
            .block(Block::default().borders(Borders::ALL).title(" actions ")),
        mid[1],
    );

    f.render_widget(
        Paragraph::new("  ←↑↓→ / hjkl move   ·   [ ] prev/next node   ·   q quit")
            .style(Style::default().fg(Color::DarkGray)),
        rows[3],
    );
}

/// Open the TUI on `spots`: walk the cursor over the grid, cycle nodes with
/// `[`/`]`, quit with `q`/Esc. Restores the terminal on exit (and on panic, via
/// ratatui's hook).
pub fn run(spots: &[SolvedSpot]) {
    if spots.is_empty() {
        eprintln!("No solved spots to show — run `cargo run -p solve-gen` or pass --board.");
        return;
    }
    if !std::io::stdout().is_terminal() {
        eprintln!("`table` draws an interactive color grid — run it in a terminal, not piped.");
        return;
    }

    let mut terminal = ratatui::init();
    let mut node = 0usize;
    let mut cursor = (0usize, 0usize);
    let mut grid = build_grid(&spots[node]);

    loop {
        let _ = terminal.draw(|f| draw(f, &spots[node], &grid, cursor, (node, spots.len())));

        let Ok(Event::Key(key)) = event::read() else {
            continue;
        };
        if key.kind != KeyEventKind::Press {
            continue; // ignore key-release (Windows fires both)
        }
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => break,
            KeyCode::Up | KeyCode::Char('k') => cursor.0 = cursor.0.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => cursor.0 = (cursor.0 + 1).min(12),
            KeyCode::Left | KeyCode::Char('h') => cursor.1 = cursor.1.saturating_sub(1),
            KeyCode::Right | KeyCode::Char('l') => cursor.1 = (cursor.1 + 1).min(12),
            KeyCode::Char(']') | KeyCode::Tab => {
                node = (node + 1) % spots.len();
                grid = build_grid(&spots[node]);
            }
            KeyCode::Char('[') | KeyCode::BackTab => {
                node = (node + spots.len() - 1) % spots.len();
                grid = build_grid(&spots[node]);
            }
            _ => {}
        }
    }

    ratatui::restore();
}

// ---- Tree-walking mode (P4 / design doc 03 M1) -----------------------------

/// Suit rows of the chance-node card picker, low→high like solver card IDs.
const SUITS: &[u8] = b"cdhs";

/// The picker cell at `(suit_row, rank_col)` as a card string, e.g. `"Ah"`.
fn picker_card(pick: (usize, usize)) -> String {
    format!("{}{}", RANKS[pick.1] as char, SUITS[pick.0] as char)
}

/// The 13×4 card picker: ranks across, suits down, dead cards dimmed. With
/// `runouts` set (the `o` view), each card is colored by the next node's
/// aggregate strategy after that card falls.
fn picker_lines(
    dealable: &[String],
    pick: (usize, usize),
    runouts: Option<&[RunoutSummary]>,
) -> Vec<Line<'static>> {
    let dim = Style::default().fg(Color::DarkGray);
    let title = if runouts.is_some() {
        "Strategy per runout — Enter descends:"
    } else {
        "Pick the next card:"
    };
    let mut lines = vec![Line::from(title), Line::default()];
    for row in 0..SUITS.len() {
        let mut spans = vec![Span::raw("  ")];
        for col in 0..RANKS.len() {
            let card = picker_card((row, col));
            let live = dealable.contains(&card);
            let picked = pick == (row, col);
            let summary = runouts.and_then(|rs| rs.iter().find(|r| r.card == card));
            match summary {
                Some(r) if live => {
                    // Per-char spans so the 4-wide cell splits by action mix.
                    let text: Vec<char> = format!(" {card} ").chars().collect();
                    let widths = segment_widths(&r.freqs, text.len());
                    let mut bg = vec![Color::Rgb(40, 40, 40); text.len()];
                    let mut pos = 0;
                    for (i, &w) in widths.iter().enumerate() {
                        let col = action_color(&r.actions[i], i, r.actions.len());
                        for _ in 0..w {
                            bg[pos] = col;
                            pos += 1;
                        }
                    }
                    for (p, ch) in text.iter().enumerate() {
                        let mut style = Style::default().bg(bg[p]).fg(Color::White);
                        if picked {
                            style = style.add_modifier(Modifier::BOLD | Modifier::UNDERLINED);
                        }
                        spans.push(Span::styled(ch.to_string(), style));
                    }
                }
                _ => {
                    let mut style = if live {
                        Style::default().fg(Color::White)
                    } else {
                        dim
                    };
                    if picked {
                        style = style.add_modifier(Modifier::BOLD | Modifier::UNDERLINED);
                    }
                    spans.push(Span::styled(format!(" {card} "), style));
                }
            }
        }
        lines.push(Line::from(spans));
    }
    lines
}

/// Side-panel detail for the picked card in the runouts view.
fn runout_detail_lines(runouts: &[RunoutSummary], pick: (usize, usize)) -> Vec<Line<'static>> {
    let card = picker_card(pick);
    let Some(r) = runouts.iter().find(|r| r.card == card) else {
        return vec![Line::from(format!("{card} can't be dealt here"))];
    };
    let mut lines = vec![Line::from(vec![
        Span::styled(card, Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(format!("   next player ev {:+.2}bb", r.ev_bb)),
    ])];
    for (i, action) in r.actions.iter().enumerate() {
        lines.push(Line::from(vec![
            Span::styled(
                "██ ",
                Style::default().fg(action_color(action, i, r.actions.len())),
            ),
            Span::raw(format!("{:<12} {:>5.1}%", action, r.freqs[i] * 100.0)),
        ]));
    }
    lines
}

/// The bucket-filter cycle for the `f` key: strong → weak → off.
fn next_bucket(cur: Option<Bucket>) -> Option<Bucket> {
    const ORDER: [Bucket; 6] = [
        Bucket::Value,
        Bucket::Overpair,
        Bucket::TopPair,
        Bucket::Pair,
        Bucket::Draw,
        Bucket::Air,
    ];
    match cur {
        None => Some(ORDER[0]),
        Some(b) => ORDER
            .iter()
            .position(|&x| x == b)
            .and_then(|i| ORDER.get(i + 1).copied()),
    }
}

/// The grid block's title: the active lens (with its scale) and filter.
fn lens_title(lens: Lens, filter: Option<Bucket>, grid: &Grid) -> String {
    let base = match lens {
        Lens::Strategy => " strategy ".to_string(),
        Lens::Range => " range · bright = reaches ".to_string(),
        Lens::Ev => {
            let (lo, hi) = ev_range(grid);
            format!(" ev · {lo:+.1} … {hi:+.1}bb ")
        }
        Lens::Equity => " equity · red 0% … green 100% ".to_string(),
        Lens::Delta => " ev Δ vs. baseline · red down … green up ".to_string(),
    };
    match filter {
        Some(b) => format!("{base}· filter {b} "),
        None => base,
    }
}

/// Browse state for the tree TUI, separate from the node it renders.
struct TreeView {
    grid: Grid,
    cursor: (usize, usize),
    pick: (usize, usize),
    lens: Lens,
    filter: Option<Bucket>,
    runouts: Option<Vec<RunoutSummary>>,
    /// P10: `L` toggles lock-edit mode; number keys then set the focused cell.
    lock_mode: bool,
    /// Pending per-cell locks for the current node (applied on `R` resolve).
    locks: CellLocks,
    /// Pre-resolve per-cell EVs, for the delta lens after `R`.
    baseline_ev: Option<HashMap<(usize, usize), f32>>,
    /// P8: villain's continue range vs. this node's biggest bet, for the
    /// side panel's blocker line.
    blockers: Option<Blockers>,
    /// One-line status shown in place of the help row (e.g. "Saved …").
    notice: Option<String>,
}

/// A saved nodelock (design doc 06): the line it applies at plus the cell
/// edits, as written by `S` in the lock editor and loaded by `--locks`.
#[derive(Debug, Serialize, Deserialize)]
pub struct LockFile {
    pub v: u32,
    /// Board at the locked node, e.g. `["Td","9d","6h","2c"]`.
    pub board: Vec<String>,
    /// Action labels root → locked node, in `--line` format.
    pub line: Vec<String>,
    /// `SpotConfig::hash8` of the solve this was saved from; mismatches warn.
    pub config_hash: String,
    pub locks: Vec<LockEntry>,
}

/// One locked grid cell: `(row, col)` plus its per-action frequencies.
#[derive(Debug, Serialize, Deserialize)]
pub struct LockEntry {
    pub row: usize,
    pub col: usize,
    pub freqs: Vec<f32>,
}

impl LockFile {
    pub fn load(path: &Path) -> std::io::Result<Self> {
        let s = std::fs::read_to_string(path)?;
        serde_json::from_str(&s).map_err(|e| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("{}: {e}", path.display()),
            )
        })
    }
}

/// Lock-file plumbing for [`run_tree`]: where `S` saves, and a file already
/// loaded (and line-descended) by the caller to apply on startup.
pub struct LockArgs {
    pub path: Option<PathBuf>,
    pub loaded: Option<LockFile>,
    pub config_hash: String,
}

/// Default save target when `--locks` wasn't given: board + line slug in cwd.
fn auto_lock_path(node: &TreeNode) -> PathBuf {
    let slug: Vec<String> = node
        .line
        .iter()
        .map(|step| {
            step.chars()
                .filter(|c| c.is_ascii_alphanumeric() || *c == '.')
                .collect::<String>()
                .to_lowercase()
        })
        .collect();
    let board = node.board.join("").to_lowercase();
    PathBuf::from(if slug.is_empty() {
        format!("{board}.locks.json")
    } else {
        format!("{board}-{}.locks.json", slug.join("-"))
    })
}

/// Lock presets (design doc 06): whole-node cell edits derived from each
/// cell's current mix, reviewed in lock mode and applied by `R` as usual.
/// `Overfold` scales every cell's fold frequency ×1.5 (capped at 1) and
/// renormalizes the rest; `NeverRaise` zeroes bet/raise actions and
/// renormalizes, dumping pure-raise cells on the first passive action.
#[derive(Clone, Copy)]
enum Preset {
    Overfold,
    NeverRaise,
}

fn preset_locks(grid: &Grid, preset: Preset) -> CellLocks {
    let mut out = CellLocks::new();
    for (r, row) in grid.iter().enumerate() {
        for (c, cell) in row.iter().enumerate() {
            let Some(cell) = cell.as_ref() else { continue };
            let mut freqs = cell.freqs.clone();
            match preset {
                Preset::Overfold => {
                    let Some(fold) = cell.actions.iter().position(|a| a == "Fold") else {
                        continue;
                    };
                    let f = freqs[fold];
                    if f <= 0.0 {
                        continue; // never-folding cells have nothing to scale
                    }
                    let nf = (f * 1.5).min(1.0);
                    let scale = if f < 1.0 { (1.0 - nf) / (1.0 - f) } else { 1.0 };
                    for (i, x) in freqs.iter_mut().enumerate() {
                        *x = if i == fold { nf } else { *x * scale };
                    }
                }
                Preset::NeverRaise => {
                    let mass: f32 = cell
                        .actions
                        .iter()
                        .zip(&freqs)
                        .filter(|(a, _)| is_aggressive(a))
                        .map(|(_, f)| f)
                        .sum();
                    if mass <= 0.0 {
                        continue; // already never raises
                    }
                    for (a, x) in cell.actions.iter().zip(freqs.iter_mut()) {
                        if is_aggressive(a) {
                            *x = 0.0;
                        }
                    }
                    let rest = 1.0 - mass;
                    if rest > 1e-6 {
                        for x in &mut freqs {
                            *x /= rest;
                        }
                    } else {
                        // Pure-raise cell: move it to the first passive action.
                        let Some(p) = cell.actions.iter().position(|a| !is_aggressive(a)) else {
                            continue;
                        };
                        freqs[p] = 1.0;
                    }
                }
            }
            out.insert((r, c), freqs);
        }
    }
    out
}

/// Villain's continuing range after hero's biggest aggressive action at the
/// current node: each villain combo with its continue mass
/// (reach × (1 − fold frequency)).
pub struct Blockers {
    /// The hero action villain is responding to, e.g. `"Bet 4.5bb"`.
    pub action: String,
    pub mass: Vec<([Card; 2], f32)>,
}

impl Blockers {
    /// Fraction of villain's continue mass that `hero` blocks by card removal.
    pub fn blocked(&self, hero: [Card; 2]) -> f32 {
        let total: f32 = self.mass.iter().map(|(_, m)| m).sum();
        if total <= 0.0 {
            return 0.0;
        }
        let dead: f32 = self
            .mass
            .iter()
            .filter(|(v, _)| v.contains(&hero[0]) || v.contains(&hero[1]))
            .map(|(_, m)| m)
            .sum();
        dead / total
    }
}

/// Fetch the villain response to hero's biggest bet/raise via a passive
/// [`TreeWalk::peek`] (design doc 03). `None` when the node has no aggressive
/// action, no villain decision follows it, or — on a table-backed walk — the
/// child is off the stored frontier (the lens quietly disappears rather than
/// paying a live solve on every navigation); errors mean the session died.
// ponytail: "continue" is defined vs. the biggest bet only; a per-action
// blocker readout would fetch one villain node per size.
fn villain_continues(
    session: &mut dyn TreeWalk,
    node: &TreeNode,
) -> std::io::Result<Option<Blockers>> {
    if node.player != "oop" && node.player != "ip" {
        return Ok(None);
    }
    let Some(i) = node.actions.iter().rposition(|a| is_aggressive(a)) else {
        return Ok(None);
    };
    let Some(vnode) = session.peek(i)? else {
        return Ok(None);
    };
    if vnode.player != "oop" && vnode.player != "ip" {
        return Ok(None);
    }
    let fold = vnode.actions.iter().position(|a| a == "Fold");
    let mass = vnode
        .hands
        .iter()
        .enumerate()
        .filter_map(|(j, hand)| {
            let reach = vnode.weights.get(j).copied().unwrap_or(1.0);
            let cont = fold.map_or(1.0, |f| 1.0 - vnode.freqs[f][j]);
            parse_hole(hand).map(|h| (h, reach * cont))
        })
        .collect();
    Ok(Some(Blockers {
        action: node.actions[i].clone(),
        mass,
    }))
}

/// The numbered action bar for a player node: `1 Check   2 Bet 3.3bb   …`.
fn action_bar(node: &TreeNode) -> Line<'static> {
    let mut spans = Vec::new();
    for (i, action) in node.actions.iter().enumerate() {
        spans.push(Span::styled(
            format!("{} ", i + 1),
            Style::default().add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::styled(
            format!("{action}   "),
            Style::default().fg(action_color(action, i, node.actions.len())),
        ));
    }
    Line::from(spans)
}

fn draw_tree(f: &mut Frame, node: &TreeNode, view: &TreeView) {
    let rows = Layout::vertical([
        Constraint::Length(5),  // breadcrumb + board/pot + action bar
        Constraint::Length(16), // grid or card picker
        Constraint::Min(5),     // detail | legend
        Constraint::Length(1),  // help
    ])
    .split(f.area());

    let breadcrumb = if node.line.is_empty() {
        "(root)".to_string()
    } else {
        node.line.join(" · ")
    };
    let to_act = match node.player.as_str() {
        "oop" => "OOP (BB) to act",
        "ip" => "IP (BTN) to act",
        "chance" => "dealing",
        _ => "terminal",
    };
    let header = Paragraph::new(vec![
        Line::from(Span::styled(
            breadcrumb,
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from(format!(
            "Board {}   Pot {:.1}bb   {}",
            fmt_hand_str(&node.board.join("")),
            node.pot_bb,
            to_act
        )),
        action_bar(node),
    ])
    .block(Block::default().borders(Borders::ALL).title(" tree "));
    f.render_widget(header, rows[0]);

    let at_runouts = node.player == "chance" && view.runouts.is_some();
    let (body, body_title): (Vec<Line>, String) = match node.player.as_str() {
        "chance" => (
            picker_lines(&node.dealable, view.pick, view.runouts.as_deref()),
            if at_runouts { " runouts " } else { " runout " }.to_string(),
        ),
        "terminal" => (
            vec![Line::from("Terminal node — u to back up, r for root.")],
            " end of line ".to_string(),
        ),
        _ => (
            grid_lines(
                &view.grid,
                view.cursor,
                view.lens,
                view.filter,
                view.baseline_ev.as_ref(),
            ),
            if view.lock_mode {
                format!(
                    " LOCK EDIT · {} cell(s) · 1-9 set, c clear, R resolve ",
                    view.locks.len()
                )
            } else {
                lens_title(view.lens, view.filter, &view.grid)
            },
        ),
    };
    f.render_widget(
        Paragraph::new(body).block(Block::default().borders(Borders::ALL).title(body_title)),
        rows[1],
    );

    let mid =
        Layout::horizontal([Constraint::Percentage(55), Constraint::Percentage(45)]).split(rows[2]);
    let (detail, legend) = match &view.runouts {
        Some(runouts) if node.player == "chance" => (
            runout_detail_lines(runouts, view.pick),
            runouts.first().map_or_else(
                || vec![Line::from("(no data)")],
                |r| action_legend(&r.actions),
            ),
        ),
        _ => (
            detail_lines(
                &view.grid,
                view.cursor,
                view.lock_mode.then_some(&view.locks),
                view.blockers.as_ref(),
            ),
            legend_lines(&view.grid),
        ),
    };
    f.render_widget(
        Paragraph::new(detail).block(Block::default().borders(Borders::ALL).title(" hand ")),
        mid[0],
    );
    f.render_widget(
        Paragraph::new(legend).block(Block::default().borders(Borders::ALL).title(" actions ")),
        mid[1],
    );

    let help = if let Some(n) = &view.notice {
        n.as_str()
    } else if node.player == "chance" {
        "  ←↑↓→ / hjkl pick   ·   Enter deal   ·   o runouts   ·   u up   r root   ·   q quit"
    } else if view.lock_mode {
        "  hjkl move · 1-9 lock cell · o overfold · n no-raise · c clear · S save · R resolve · L/Esc cancel"
    } else {
        "  ←↑↓→ move · 1-9 act · s/w/e/y/d lens · f filter · L lock · u up · r root · q quit"
    };
    let help = help.to_string();
    f.render_widget(
        Paragraph::new(help).style(Style::default().fg(Color::DarkGray)),
        rows[3],
    );
}

/// Lock the current node's `strategy` and re-solve, forcing a live session
/// (a disk-backed table can't re-solve, and is stale afterward anyway).
fn lock_and_resolve(
    session: &mut dyn TreeWalk,
    strategy: &[Vec<f32>],
) -> std::io::Result<TreeNode> {
    let live = session.live()?;
    live.lock(strategy)?;
    live.resolve()
}

/// Walk a solved game tree: numbered actions descend, `u`/`r` go up, and chance
/// nodes offer a card picker. `session` is a [`TreeWalk`] — a live tree session
/// or a reach-pruned table that live-solves off its stored path — held by the
/// caller for the whole browse; a dead child ends the TUI with its error.
pub fn run_tree(session: &mut dyn TreeWalk, mut node: TreeNode, mut lock_args: LockArgs) {
    if !std::io::stdout().is_terminal() {
        eprintln!("`table` draws an interactive color grid — run it in a terminal, not piped.");
        return;
    }

    // A loaded lock file replays the saved edits before the TUI opens: same
    // flow as `R`, so the browser starts on the delta lens vs. the unlocked
    // baseline. The caller already descended to the file's line.
    let (mut init_locks, mut init_baseline, mut init_lens) =
        (CellLocks::new(), None, Lens::Strategy);
    if let Some(f) = lock_args.loaded.take() {
        let cells: CellLocks = f
            .locks
            .into_iter()
            .filter(|e| e.row < 13 && e.col < 13 && e.freqs.len() == node.actions.len())
            .map(|e| ((e.row, e.col), e.freqs))
            .collect();
        if cells.is_empty() {
            eprintln!("Lock file has no locks applicable at this node — ignoring it.");
        } else {
            eprintln!("Applying {} saved cell locks and re-solving…", cells.len());
            let grid = build_grid_node(&node);
            let strategy = expand_lock(&node, &cells);
            match lock_and_resolve(session, &strategy) {
                Ok(next) => {
                    init_baseline = Some(baseline_map(&grid));
                    init_locks = cells;
                    init_lens = Lens::Delta;
                    node = next;
                }
                Err(e) => {
                    eprintln!("Applying the lock file failed: {e}");
                    return;
                }
            }
        }
    }

    let mut terminal = ratatui::init();
    let mut view = TreeView {
        grid: build_grid_node(&node),
        cursor: (0, 0),
        pick: (0, 0),
        lens: init_lens,
        filter: None,
        runouts: None,
        lock_mode: false,
        locks: init_locks,
        baseline_ev: init_baseline,
        blockers: villain_continues(session, &node).unwrap_or(None),
        notice: None,
    };
    let mut died: Option<std::io::Error> = None;

    loop {
        let _ = terminal.draw(|f| draw_tree(f, &node, &view));

        let Ok(Event::Key(key)) = event::read() else {
            continue;
        };
        if key.kind != KeyEventKind::Press {
            continue;
        }
        view.notice = None;
        let at_chance = node.player == "chance";
        // Cursor keys move the grid cursor, or the card picker at chance nodes.
        let (pos, max) = if at_chance {
            (&mut view.pick, (SUITS.len() - 1, RANKS.len() - 1))
        } else {
            (&mut view.cursor, (12, 12))
        };
        let nav = match key.code {
            KeyCode::Char('q') => break,
            // Esc backs out of lock-edit mode first; otherwise it quits.
            KeyCode::Esc => {
                if view.lock_mode {
                    view.lock_mode = false;
                    None
                } else {
                    break;
                }
            }
            KeyCode::Up | KeyCode::Char('k') => {
                pos.0 = pos.0.saturating_sub(1);
                None
            }
            KeyCode::Down | KeyCode::Char('j') => {
                pos.0 = (pos.0 + 1).min(max.0);
                None
            }
            KeyCode::Left | KeyCode::Char('h') => {
                pos.1 = pos.1.saturating_sub(1);
                None
            }
            KeyCode::Right | KeyCode::Char('l') => {
                pos.1 = (pos.1 + 1).min(max.1);
                None
            }
            KeyCode::Char('u') => Some(session.back()),
            KeyCode::Char('r') => Some(session.root()),
            KeyCode::Enter if at_chance => {
                let card = picker_card(view.pick);
                node.dealable.contains(&card).then(|| session.deal(&card))
            }
            // Lock mode: number keys set the focused cell to a pure action.
            KeyCode::Char(c @ '1'..='9') if !at_chance && view.lock_mode => {
                let i = c as usize - '1' as usize;
                if i < node.actions.len() {
                    let mut freqs = vec![0.0; node.actions.len()];
                    freqs[i] = 1.0;
                    view.locks.insert(view.cursor, freqs);
                }
                None
            }
            KeyCode::Char(c @ '1'..='9') if !at_chance => {
                let i = c as usize - '1' as usize;
                (i < node.actions.len()).then(|| session.play(i))
            }
            KeyCode::Char('L') if !at_chance && !node.actions.is_empty() => {
                view.lock_mode = !view.lock_mode;
                if view.lock_mode {
                    view.locks.clear();
                }
                None
            }
            // Lock-mode presets: whole-node cell edits from the current mix.
            KeyCode::Char('o') if view.lock_mode => {
                view.locks = preset_locks(&view.grid, Preset::Overfold);
                None
            }
            KeyCode::Char('n') if view.lock_mode => {
                view.locks = preset_locks(&view.grid, Preset::NeverRaise);
                None
            }
            KeyCode::Char('S') if view.lock_mode && !view.locks.is_empty() => {
                let path = lock_args
                    .path
                    .clone()
                    .unwrap_or_else(|| auto_lock_path(&node));
                let file = LockFile {
                    v: 1,
                    board: node.board.clone(),
                    line: node.line.clone(),
                    config_hash: lock_args.config_hash.clone(),
                    locks: view
                        .locks
                        .iter()
                        .map(|(&(row, col), freqs)| LockEntry {
                            row,
                            col,
                            freqs: freqs.clone(),
                        })
                        .collect(),
                };
                view.notice = Some(
                    match serde_json::to_string_pretty(&file)
                        .map_err(std::io::Error::from)
                        .and_then(|s| std::fs::write(&path, s))
                    {
                        Ok(()) => format!(
                            "  Saved {} cell locks to {} (reload with --locks).",
                            file.locks.len(),
                            path.display()
                        ),
                        Err(e) => format!("  Saving locks to {} failed: {e}", path.display()),
                    },
                );
                None
            }
            KeyCode::Char('c') if view.lock_mode => {
                view.locks.remove(&view.cursor);
                None
            }
            KeyCode::Char('R') if !at_chance && !view.locks.is_empty() => {
                // Send the expanded lock, re-solve, and switch to the delta lens
                // vs. the strategy we're leaving (captured now, pre-rebuild).
                let base = baseline_map(&view.grid);
                let strategy = expand_lock(&node, &view.locks);
                match lock_and_resolve(session, &strategy) {
                    Ok(next) => {
                        node = next;
                        view.grid = build_grid_node(&node);
                        view.baseline_ev = Some(base);
                        view.lens = Lens::Delta;
                        view.lock_mode = false;
                        view.runouts = None;
                        match villain_continues(session, &node) {
                            Ok(b) => view.blockers = b,
                            Err(e) => {
                                died = Some(e);
                                break;
                            }
                        }
                    }
                    Err(e) => {
                        died = Some(e);
                        break;
                    }
                }
                None
            }
            KeyCode::Char('d') => {
                view.lens = Lens::Delta;
                None
            }
            KeyCode::Char('s') => {
                view.lens = Lens::Strategy;
                None
            }
            KeyCode::Char('w') => {
                view.lens = Lens::Range;
                None
            }
            KeyCode::Char('e') => {
                view.lens = Lens::Ev;
                None
            }
            KeyCode::Char('y') => {
                view.lens = Lens::Equity;
                None
            }
            KeyCode::Char('f') => {
                view.filter = next_bucket(view.filter);
                None
            }
            KeyCode::Char('o') if at_chance => {
                if view.runouts.take().is_none() {
                    match session.runouts() {
                        Ok(rs) => view.runouts = Some(rs),
                        Err(e) => {
                            died = Some(e);
                            break;
                        }
                    }
                }
                None
            }
            _ => None,
        };
        match nav {
            Some(Ok(next)) => {
                node = next;
                view.grid = build_grid_node(&node);
                view.runouts = None;
                // Locks + the delta baseline are per-node; drop them on a move.
                view.locks.clear();
                view.lock_mode = false;
                view.baseline_ev = None;
                if view.lens == Lens::Delta {
                    view.lens = Lens::Strategy;
                }
                match villain_continues(session, &node) {
                    Ok(b) => view.blockers = b,
                    Err(e) => {
                        died = Some(e);
                        break;
                    }
                }
            }
            Some(Err(e)) => {
                died = Some(e);
                break;
            }
            None => {}
        }
    }

    ratatui::restore();
    if let Some(e) = died {
        eprintln!("Tree session died: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::solution::{HandStrategy, NodeStrategy, SolvedSpot};

    fn hole(s: &str) -> [Card; 2] {
        parse_hole(s).unwrap()
    }

    #[test]
    fn canonical_places_pairs_suited_offsuit() {
        assert_eq!(canonical(&hole("AsAh")).unwrap(), ("AA".into(), 0, 0));
        assert_eq!(canonical(&hole("AsKs")).unwrap(), ("AKs".into(), 0, 1)); // suited, upper-right
        assert_eq!(canonical(&hole("AsKh")).unwrap(), ("AKo".into(), 1, 0)); // offsuit, lower-left
        assert_eq!(canonical(&hole("KhAs")).unwrap(), ("AKo".into(), 1, 0)); // order-independent
        assert_eq!(canonical(&hole("2c3d")).unwrap(), ("32o".into(), 12, 11));
    }

    #[test]
    fn build_grid_averages_combos_into_one_cell() {
        let mk = |hand: &str, f: Vec<f32>| HandStrategy {
            hand: hand.into(),
            strategy: NodeStrategy {
                actions: vec!["Check".into(), "Bet 2.0bb".into()],
                frequencies: f,
                action_ev: vec![0.0, 0.0],
            },
        };
        let spot = SolvedSpot {
            label: "t".into(),
            board: vec!["2c".into(), "7d".into(), "9h".into()],
            pot_bb: 6.0,
            hero_oop: false,
            villain_action: "x".into(),
            config: None,
            generator: None,
            strategies: vec![mk("AsKs", vec![0.2, 0.8]), mk("AhKh", vec![0.4, 0.6])],
        };
        let grid = build_grid(&spot);
        let cell = grid[0][1].as_ref().unwrap(); // AKs
        assert_eq!(cell.label, "AKs");
        assert_eq!(cell.combos, 2);
        assert!((cell.freqs[0] - 0.3).abs() < 1e-6);
        assert!((cell.freqs[1] - 0.7).abs() < 1e-6);
    }

    #[test]
    fn segment_widths_fill_the_cell() {
        assert_eq!(
            segment_widths(&[0.2, 0.3, 0.5], CELL_W)
                .iter()
                .sum::<usize>(),
            CELL_W
        );
        assert_eq!(segment_widths(&[1.0], CELL_W), vec![CELL_W]);
        assert_eq!(segment_widths(&[], CELL_W), Vec::<usize>::new());
    }

    #[test]
    fn picker_card_maps_rows_and_cols() {
        assert_eq!(picker_card((0, 0)), "Ac"); // top-left: ace of clubs
        assert_eq!(picker_card((3, 12)), "2s"); // bottom-right: deuce of spades
        assert_eq!(picker_card((2, 4)), "Th");
    }

    fn tree_node() -> TreeNode {
        TreeNode {
            player: "ip".into(),
            board: vec!["Td".into(), "9d".into(), "6h".into()],
            pot_bb: 6.0,
            line: vec!["Check".into()],
            actions: vec!["Check".into(), "Bet 2.0bb".into()],
            hands: vec!["AsKs".into(), "AhKh".into(), "2s2d".into()],
            freqs: vec![vec![0.4, 0.2, 1.0], vec![0.6, 0.8, 0.0]],
            evs: vec![vec![1.0, 1.2, 0.1], vec![2.0, 2.4, -0.5]],
            weights: vec![1.0, 0.5, 0.0],
            equity: vec![0.6, 0.7, 0.3],
            ..Default::default()
        }
    }

    #[test]
    fn build_grid_node_keeps_weights_equity_and_best_ev() {
        let grid = build_grid_node(&tree_node());
        let aks = grid[0][1].as_ref().unwrap();
        assert_eq!(aks.combos, 2);
        // Reach-weighted means over weights [1.0, 0.5].
        assert!((aks.weight - 0.75).abs() < 1e-6);
        assert!((aks.equity - (0.6 + 0.5 * 0.7) / 1.5).abs() < 1e-6);
        assert!((aks.ev - (2.0 + 0.5 * 2.4) / 1.5).abs() < 1e-6); // best action per combo
        assert!((aks.freqs[1] - (0.6 + 0.5 * 0.8) / 1.5).abs() < 1e-6);
        assert_eq!(aks.rows.len(), 2);
        // AKs has no made pair on Td9d6h and no draw -> Air.
        assert_eq!(aks.rows[0].bucket, Some(Bucket::Air));

        // A zero-reach cell falls back to the plain mean so its bar still shows.
        let pair = grid[12][12].as_ref().unwrap(); // 22
        assert!((pair.freqs[0] - 1.0).abs() < 1e-6);
        assert_eq!(pair.weight, 0.0);
    }

    #[test]
    fn snapshot_grid_carries_no_equity() {
        let mk = |hand: &str, f: Vec<f32>| HandStrategy {
            hand: hand.into(),
            strategy: NodeStrategy {
                actions: vec!["Check".into(), "Bet 2.0bb".into()],
                frequencies: f,
                action_ev: vec![0.0, 0.0],
            },
        };
        let spot = SolvedSpot {
            label: "t".into(),
            board: vec!["Td".into(), "9d".into(), "6h".into()],
            pot_bb: 6.0,
            hero_oop: false,
            villain_action: "x".into(),
            config: None,
            generator: None,
            strategies: vec![mk("AsKs", vec![0.4, 0.6]), mk("AhKh", vec![0.2, 0.8])],
        };
        let grid = build_grid(&spot);
        let aks = grid[0][1].as_ref().unwrap();
        assert!(aks.equity.is_nan());
        assert_eq!(aks.weight, 1.0);
        // Unweighted mean, as in v1.
        assert!((aks.freqs[1] - 0.7).abs() < 1e-6);
    }

    #[test]
    fn bucket_filter_cycles_and_matches() {
        assert_eq!(next_bucket(None), Some(Bucket::Value));
        assert_eq!(next_bucket(Some(Bucket::Air)), None);
        let grid = build_grid_node(&tree_node());
        let aks = grid[0][1].as_ref().unwrap();
        assert_eq!(bucket_frac(aks, Bucket::Air), 1.0);
        assert_eq!(bucket_frac(aks, Bucket::Value), 0.0);
    }

    #[test]
    fn expand_lock_fills_locked_cells_and_zeros_the_rest() {
        // tree_node(): AsKs/AhKh → AKs cell (0,1), 2s2d → 22 cell (12,12).
        let node = tree_node();
        let mut locks = CellLocks::new();
        locks.insert((0, 1), vec![0.0, 1.0]); // AKs: always the second action
        let strat = expand_lock(&node, &locks);
        // Shape is [action][hand], parallel to node.freqs.
        assert_eq!(strat.len(), node.actions.len());
        assert_eq!(strat[0].len(), node.hands.len());
        // Both AK combos (hands 0,1) get the locked freqs; 22 (hand 2) stays 0.
        for j in [0, 1] {
            assert_eq!(strat[0][j], 0.0);
            assert_eq!(strat[1][j], 1.0);
        }
        assert_eq!(strat[0][2], 0.0);
        assert_eq!(strat[1][2], 0.0);
    }

    #[test]
    fn blocked_is_the_dead_share_of_continue_mass() {
        let b = Blockers {
            action: "Bet 2.0bb".into(),
            // Continue mass 1.0 + 0.5; QQ folds always so it carries none.
            mass: vec![
                (hole("AhAd"), 1.0),
                (hole("KsKd"), 0.5),
                (hole("QsQd"), 0.0),
            ],
        };
        // AhKs blocks both live combos -> 100%.
        assert!((b.blocked(hole("AhKs")) - 1.0).abs() < 1e-6);
        // KdQs blocks only KsKd -> 0.5 / 1.5.
        assert!((b.blocked(hole("KdQs")) - 0.5 / 1.5).abs() < 1e-6);
        // 7c2c blocks nothing.
        assert_eq!(b.blocked(hole("7c2c")), 0.0);
    }

    fn one_cell_grid(actions: &[&str], freqs: &[f32]) -> Grid {
        let mut g: Grid = std::array::from_fn(|_| std::array::from_fn(|_| None));
        g[0][0] = Some(Cell {
            label: "AA".into(),
            combos: 1,
            actions: actions.iter().map(|s| s.to_string()).collect(),
            freqs: freqs.to_vec(),
            weight: 1.0,
            equity: 0.5,
            ev: 0.0,
            rows: vec![],
        });
        g
    }

    #[test]
    fn presets_rescale_the_cell_mix() {
        // Overfold ×1.5: fold .4 -> .6, the rest scaled by (1-.6)/(1-.4).
        let g = one_cell_grid(&["Fold", "Call", "Raise to 6.0bb"], &[0.4, 0.4, 0.2]);
        let locks = preset_locks(&g, Preset::Overfold);
        let f = &locks[&(0, 0)];
        assert!((f[0] - 0.6).abs() < 1e-6);
        assert!((f[1] - 0.4 * (2.0 / 3.0)).abs() < 1e-6);
        assert!((f[2] - 0.2 * (2.0 / 3.0)).abs() < 1e-6);

        // A never-folding cell has nothing to overfold; no Fold action, no locks.
        assert!(preset_locks(
            &one_cell_grid(&["Fold", "Call"], &[0.0, 1.0]),
            Preset::Overfold
        )
        .is_empty());
        assert!(preset_locks(
            &one_cell_grid(&["Check", "Bet"], &[0.5, 0.5]),
            Preset::Overfold
        )
        .is_empty());

        // Never-raise zeroes aggression and renormalizes the rest.
        let g = one_cell_grid(&["Fold", "Call", "Bet 2.0bb"], &[0.2, 0.3, 0.5]);
        let f = &preset_locks(&g, Preset::NeverRaise)[&(0, 0)];
        assert!((f[0] - 0.4).abs() < 1e-6);
        assert!((f[1] - 0.6).abs() < 1e-6);
        assert_eq!(f[2], 0.0);

        // A pure-raise cell dumps onto the first passive action.
        let g = one_cell_grid(&["Fold", "Call", "Raise to 9.0bb"], &[0.0, 0.0, 1.0]);
        let f = &preset_locks(&g, Preset::NeverRaise)[&(0, 0)];
        assert_eq!(f, &vec![1.0, 0.0, 0.0]);
    }

    #[test]
    fn lock_file_roundtrips_through_json() {
        let file = LockFile {
            v: 1,
            board: vec!["Td".into(), "9d".into(), "6h".into()],
            line: vec!["Check".into(), "Bet 2.0bb".into()],
            config_hash: "f55543b1".into(),
            locks: vec![LockEntry {
                row: 0,
                col: 1,
                freqs: vec![0.25, 0.75],
            }],
        };
        let json = serde_json::to_string(&file).unwrap();
        let back: LockFile = serde_json::from_str(&json).unwrap();
        assert_eq!(back.line, file.line);
        assert_eq!(back.locks[0].freqs, file.locks[0].freqs);
    }

    #[test]
    fn auto_lock_path_slugs_the_line() {
        let node = TreeNode {
            board: vec!["Td".into(), "9d".into(), "6h".into(), "2c".into()],
            line: vec!["Check".into(), "Bet 2.0bb".into(), "deal 2c".into()],
            ..Default::default()
        };
        assert_eq!(
            auto_lock_path(&node).to_str().unwrap(),
            "td9d6h2c-check-bet2.0bb-deal2c.locks.json"
        );
    }

    #[test]
    fn heat_color_hits_the_ramp_ends() {
        assert_eq!(heat_color(0.0), Color::Rgb(200, 60, 50));
        assert_eq!(heat_color(1.0), Color::Rgb(60, 165, 90));
    }

    #[test]
    fn draws_tree_frames_without_panicking() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;
        let player = tree_node();
        let chance = TreeNode {
            player: "chance".into(),
            board: vec!["Td".into(), "9d".into(), "6h".into()],
            pot_bb: 10.0,
            line: vec!["Check".into(), "Check".into()],
            dealable: vec!["2c".into(), "Ah".into()],
            ..Default::default()
        };
        let terminal_node = TreeNode {
            player: "terminal".into(),
            board: vec!["Td".into(), "9d".into(), "6h".into()],
            pot_bb: 12.0,
            line: vec!["Bet 2.0bb".into(), "Fold".into()],
            ..Default::default()
        };
        let runouts = vec![RunoutSummary {
            card: "2c".into(),
            actions: vec!["Check".into(), "Bet 5.0bb".into()],
            freqs: vec![0.6, 0.4],
            ev_bb: 1.2,
        }];
        // A baseline + a lock, so the delta lens and lock-mode UI both render.
        let mut baseline = HashMap::new();
        baseline.insert((0, 1), 5.0);
        let mut locks = CellLocks::new();
        locks.insert((0, 1), vec![1.0, 0.0]);
        let mut terminal = Terminal::new(TestBackend::new(80, 30)).unwrap();
        for node in [player, chance, terminal_node] {
            for (lens, filter, runouts, lock_mode) in [
                (Lens::Strategy, None, None, false),
                (Lens::Range, Some(Bucket::Air), None, false),
                (Lens::Ev, None, None, false),
                (Lens::Equity, None, Some(runouts.clone()), false),
                (Lens::Delta, None, None, false),
                (Lens::Strategy, None, None, true), // lock-edit mode
            ] {
                let view = TreeView {
                    grid: build_grid_node(&node),
                    cursor: (0, 1),
                    pick: (0, 12), // 2c: a dealable, summarized card
                    lens,
                    filter,
                    runouts,
                    lock_mode,
                    locks: locks.clone(),
                    baseline_ev: Some(baseline.clone()),
                    blockers: Some(Blockers {
                        action: "Bet 2.0bb".into(),
                        mass: vec![
                            (hole("AhAd"), 1.0),
                            (hole("KsKd"), 0.5),
                            (hole("QsQd"), 0.0),
                        ],
                    }),
                    notice: Some("Saved 2 cell locks to spot.locks.json".into()),
                };
                terminal.draw(|f| draw_tree(f, &node, &view)).unwrap();
            }
        }
    }

    #[test]
    fn draws_a_frame_without_panicking() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;
        let mk = |hand: &str| HandStrategy {
            hand: hand.into(),
            strategy: NodeStrategy {
                actions: vec!["Check".into(), "Bet 2.0bb".into(), "Bet 4.5bb".into()],
                frequencies: vec![0.5, 0.3, 0.2],
                action_ev: vec![1.0, 2.0, 3.0],
            },
        };
        let spot = SolvedSpot {
            label: "smoke".into(),
            board: vec!["Td".into(), "9d".into(), "6h".into()],
            pot_bb: 6.0,
            hero_oop: false,
            villain_action: "checks".into(),
            config: None,
            generator: None,
            strategies: vec![mk("AsKs"), mk("AhKh"), mk("2c2d")],
        };
        let grid = build_grid(&spot);
        let mut terminal = Terminal::new(TestBackend::new(80, 30)).unwrap();
        terminal
            .draw(|f| draw(f, &spot, &grid, (0, 1), (0, 1)))
            .unwrap();
    }
}
