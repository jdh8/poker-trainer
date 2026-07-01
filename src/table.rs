//! `poker-trainer table` — browse a solved spot's whole strategy as a
//! GTO-Wizard-style 13×13 starting-hand grid, each cell colored by its
//! equilibrium action mix.
//!
//! The data is already in [`SolvedSpot`]: one [`NodeStrategy`] per combo. This
//! module folds those ~1326 combos into the 169 canonical cells and draws them.
//! The folding + coloring are pure (and unit-tested below); the TUI half just
//! renders the grid and walks the cursor / cycles nodes.

use crate::solution::SolvedSpot;
use crate::trainer::{fmt_hand_str, parse_hole};
use crate::tree::{TreeNode, TreeSession};
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind};
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;
use rs_poker::core::Card;
use std::io::IsTerminal;

/// Ranks high→low; a card's index into this is its grid row/col (A = 0).
const RANKS: &[u8] = b"AKQJT98765432";
/// Inner width of a grid cell, in chars (fits a 3-char label + color margins).
const CELL_W: usize = 5;

/// One aggregated grid cell: a canonical hand and its combo-averaged mix.
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
}

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

/// Fold a spot's per-combo strategies into the 13×13 canonical grid, averaging
/// each cell's action frequencies over the combos that land in it.
pub fn build_grid(spot: &SolvedSpot) -> [[Option<Cell>; 13]; 13] {
    let mut grid: [[Option<Cell>; 13]; 13] = std::array::from_fn(|_| std::array::from_fn(|_| None));
    for hs in &spot.strategies {
        let Some(hole) = parse_hole(&hs.hand) else {
            continue;
        };
        let Some((label, r, c)) = canonical(&hole) else {
            continue;
        };
        let ns = &hs.strategy;
        let cell = grid[r][c].get_or_insert_with(|| Cell {
            label,
            combos: 0,
            actions: ns.actions.clone(),
            freqs: vec![0.0; ns.frequencies.len()],
        });
        // ponytail: a node shares one action set, so this guard never trips in
        // practice — it just keeps a mismatched combo from panicking the zip.
        if cell.freqs.len() == ns.frequencies.len() {
            for (acc, f) in cell.freqs.iter_mut().zip(&ns.frequencies) {
                *acc += f;
            }
            cell.combos += 1;
        }
    }
    for cell in grid.iter_mut().flatten().flatten() {
        if cell.combos > 0 {
            for f in &mut cell.freqs {
                *f /= cell.combos as f32;
            }
        }
    }
    grid
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

/// Styled spans for one cell: a `CELL_W`-wide bar split into colored segments by
/// frequency, with the hand label centered on top.
fn cell_spans(cell: &Cell, focused: bool) -> Vec<Span<'static>> {
    let widths = segment_widths(&cell.freqs, CELL_W);
    let mut bg = [Color::Rgb(40, 40, 40); CELL_W]; // unfilled remainder, dark
    let mut pos = 0;
    for (i, &w) in widths.iter().enumerate() {
        let col = action_color(&cell.actions[i], i, cell.actions.len());
        for _ in 0..w {
            if pos < CELL_W {
                bg[pos] = col;
                pos += 1;
            }
        }
    }
    let label: Vec<char> = cell.label.chars().collect();
    let start = CELL_W.saturating_sub(label.len()) / 2;
    (0..CELL_W)
        .map(|p| {
            let ch = label.get(p.wrapping_sub(start)).copied().unwrap_or(' ');
            let ch = if p >= start { ch } else { ' ' };
            let mut style = Style::default().bg(bg[p]).fg(Color::White);
            if focused {
                style = style.add_modifier(Modifier::BOLD | Modifier::UNDERLINED);
            }
            Span::styled(ch.to_string(), style)
        })
        .collect()
}

/// The grid as styled lines: a rank header row, then 13 rows of 13 cells.
fn grid_lines(grid: &[[Option<Cell>; 13]; 13], cursor: (usize, usize)) -> Vec<Line<'static>> {
    let dim = Style::default().fg(Color::DarkGray);
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
                Some(cell) => spans.extend(cell_spans(cell, cursor == (r, c))),
                None => spans.push(Span::raw(" ".repeat(CELL_W))),
            }
        }
        lines.push(Line::from(spans));
    }
    lines
}

/// The detail panel for the focused cell: hand, combo count, exact mix.
fn detail_lines(grid: &[[Option<Cell>; 13]; 13], cursor: (usize, usize)) -> Vec<Line<'static>> {
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
    for (i, action) in cell.actions.iter().enumerate() {
        lines.push(Line::from(vec![
            Span::styled(
                "██ ",
                Style::default().fg(action_color(action, i, cell.actions.len())),
            ),
            Span::raw(format!("{:<12} {:>5.1}%", action, cell.freqs[i] * 100.0)),
        ]));
    }
    lines
}

/// The action→color legend, read off any present cell (a node shares one set).
fn legend_lines(grid: &[[Option<Cell>; 13]; 13]) -> Vec<Line<'static>> {
    let Some(cell) = grid.iter().flatten().flatten().next() else {
        return vec![Line::from("(no data)")];
    };
    cell.actions
        .iter()
        .enumerate()
        .map(|(i, action)| {
            Line::from(vec![
                Span::styled(
                    "██ ",
                    Style::default().fg(action_color(action, i, cell.actions.len())),
                ),
                Span::raw(action.clone()),
            ])
        })
        .collect()
}

fn draw(
    f: &mut Frame,
    spot: &SolvedSpot,
    grid: &[[Option<Cell>; 13]; 13],
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

    let grid_widget = Paragraph::new(grid_lines(grid, cursor))
        .block(Block::default().borders(Borders::ALL).title(" strategy "));
    f.render_widget(grid_widget, rows[1]);

    let mid =
        Layout::horizontal([Constraint::Percentage(55), Constraint::Percentage(45)]).split(rows[2]);
    f.render_widget(
        Paragraph::new(detail_lines(grid, cursor))
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

/// The 13×4 card picker: ranks across, suits down, dead cards dimmed.
fn picker_lines(dealable: &[String], pick: (usize, usize)) -> Vec<Line<'static>> {
    let dim = Style::default().fg(Color::DarkGray);
    let mut lines = vec![Line::from("Pick the next card:"), Line::default()];
    for row in 0..SUITS.len() {
        let mut spans = vec![Span::raw("  ")];
        for col in 0..RANKS.len() {
            let card = picker_card((row, col));
            let live = dealable.contains(&card);
            let mut style = if live {
                Style::default().fg(Color::White)
            } else {
                dim
            };
            if pick == (row, col) {
                style = style.add_modifier(Modifier::BOLD | Modifier::UNDERLINED);
            }
            spans.push(Span::styled(format!(" {card} "), style));
        }
        lines.push(Line::from(spans));
    }
    lines
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

fn draw_tree(
    f: &mut Frame,
    node: &TreeNode,
    grid: &[[Option<Cell>; 13]; 13],
    cursor: (usize, usize),
    pick: (usize, usize),
) {
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

    let (body, body_title): (Vec<Line>, &str) = match node.player.as_str() {
        "chance" => (picker_lines(&node.dealable, pick), " runout "),
        "terminal" => (
            vec![Line::from("Terminal node — u to back up, r for root.")],
            " end of line ",
        ),
        _ => (grid_lines(grid, cursor), " strategy "),
    };
    f.render_widget(
        Paragraph::new(body).block(Block::default().borders(Borders::ALL).title(body_title)),
        rows[1],
    );

    let mid =
        Layout::horizontal([Constraint::Percentage(55), Constraint::Percentage(45)]).split(rows[2]);
    f.render_widget(
        Paragraph::new(detail_lines(grid, cursor))
            .block(Block::default().borders(Borders::ALL).title(" hand ")),
        mid[0],
    );
    f.render_widget(
        Paragraph::new(legend_lines(grid))
            .block(Block::default().borders(Borders::ALL).title(" actions ")),
        mid[1],
    );

    let help = if node.player == "chance" {
        "  ←↑↓→ / hjkl pick   ·   Enter deal   ·   u up   r root   ·   q quit"
    } else {
        "  ←↑↓→ / hjkl move   ·   1-9 act   ·   u up   r root   ·   q quit"
    };
    f.render_widget(
        Paragraph::new(help).style(Style::default().fg(Color::DarkGray)),
        rows[3],
    );
}

/// Walk a solved game tree live: numbered actions descend, `u`/`r` go up, and
/// chance nodes offer a card picker. The session (and its ~1 GB solver child)
/// lives for the whole browse; a dead child ends the TUI with its error.
pub fn run_tree(mut session: TreeSession, mut node: TreeNode) {
    if !std::io::stdout().is_terminal() {
        eprintln!("`table` draws an interactive color grid — run it in a terminal, not piped.");
        return;
    }

    let mut terminal = ratatui::init();
    let mut cursor = (0usize, 0usize);
    let mut pick = (0usize, 0usize);
    let mut grid = build_grid(&node.to_spot());
    let mut died: Option<std::io::Error> = None;

    loop {
        let _ = terminal.draw(|f| draw_tree(f, &node, &grid, cursor, pick));

        let Ok(Event::Key(key)) = event::read() else {
            continue;
        };
        if key.kind != KeyEventKind::Press {
            continue;
        }
        let at_chance = node.player == "chance";
        // Cursor keys move the grid cursor, or the card picker at chance nodes.
        let (pos, max) = if at_chance {
            (&mut pick, (SUITS.len() - 1, RANKS.len() - 1))
        } else {
            (&mut cursor, (12, 12))
        };
        let nav = match key.code {
            KeyCode::Char('q') | KeyCode::Esc => break,
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
                let card = picker_card(pick);
                node.dealable.contains(&card).then(|| session.deal(&card))
            }
            KeyCode::Char(c @ '1'..='9') if !at_chance => {
                let i = c as usize - '1' as usize;
                (i < node.actions.len()).then(|| session.play(i))
            }
            _ => None,
        };
        match nav {
            Some(Ok(next)) => {
                node = next;
                grid = build_grid(&node.to_spot());
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

    #[test]
    fn draws_tree_frames_without_panicking() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;
        let player = TreeNode {
            player: "ip".into(),
            board: vec!["Td".into(), "9d".into(), "6h".into()],
            pot_bb: 6.0,
            line: vec!["Check".into()],
            actions: vec!["Check".into(), "Bet 2.0bb".into()],
            hands: vec!["AsKs".into()],
            freqs: vec![vec![0.4], vec![0.6]],
            evs: vec![vec![1.0], vec![2.0]],
            ..Default::default()
        };
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
        let mut terminal = Terminal::new(TestBackend::new(80, 30)).unwrap();
        for node in [player, chance, terminal_node] {
            let grid = build_grid(&node.to_spot());
            terminal
                .draw(|f| draw_tree(f, &node, &grid, (0, 0), (1, 2)))
                .unwrap();
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
            strategies: vec![mk("AsKs"), mk("AhKh"), mk("2c2d")],
        };
        let grid = build_grid(&spot);
        let mut terminal = Terminal::new(TestBackend::new(80, 30)).unwrap();
        terminal
            .draw(|f| draw(f, &spot, &grid, (0, 1), (0, 1)))
            .unwrap();
    }
}
