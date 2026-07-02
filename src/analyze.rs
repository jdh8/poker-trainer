//! Analyze: hand-history import, EV-loss scoring & leak report (design doc 05, P9).
//!
//! Pipeline: parse → normalize/match → score → report. Parse reads PokerStars
//! and GGPoker text exports; normalize matches hands heads-up-by-the-flop onto
//! library formations; score replays each matched hand through a live
//! [`TreeSession`] (one solve per distinct formation × flop, bounded by
//! `--solve-budget`) and grades every hero decision on EV loss vs. the
//! equilibrium mix. Only *score* touches a solver — `--dry-run` stops after
//! the match/coverage report.

use crate::solution::{formation, Formation, SolveRequest, SpotConfig};
use crate::stats::{self, GroupBy, StatRecord};
use crate::tree::{TreeNode, TreeSession};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Depth band accepted around a formation's stack (design 05's "60–150bb →
/// 100bb config").
const STACK_BAND: (f32, f32) = (0.6, 1.5);
/// Flop-pot tolerance vs. the formation's pot.
const POT_TOLERANCE: f32 = 0.25;

/// One player action. Amounts are in table currency as printed; `RaiseTo` is
/// the street total ("raises $1.50 to $2.50" → 2.50), everything else is
/// incremental — PokerStars' own convention.
#[derive(Debug, Clone, PartialEq)]
pub enum Act {
    Fold,
    Check,
    Call(f32),
    Bet(f32),
    RaiseTo(f32),
    /// Any blind/ante post.
    Post(f32),
}

#[derive(Debug, Clone)]
pub struct Action {
    pub player: String,
    pub act: Act,
}

/// One parsed hand — only the fields the analyzer consumes (design doc 05).
/// Showdown lines are ignored: scoring needs hero's line, not the reveal.
#[derive(Debug, Default)]
pub struct RawHand {
    pub id: String,
    /// False for tournaments and non-NLHE games; a skip, not a parse error.
    pub nlhe_cash: bool,
    /// Big blind in table currency.
    pub bb: f32,
    pub button_seat: u32,
    /// (seat, name, starting stack), in seat order.
    pub seats: Vec<(u32, String, f32)>,
    pub sb_player: Option<String>,
    pub bb_player: Option<String>,
    /// The "Dealt to" player.
    pub hero: Option<String>,
    pub hero_cards: Vec<String>,
    /// All cards dealt, e.g. `["Td","9d","6h","2c","7s"]`.
    pub board: Vec<String>,
    /// Blind posts + preflop actions.
    pub preflop: Vec<Action>,
    /// Flop, turn, river actions.
    pub postflop: [Vec<Action>; 3],
}

/// Why a parsed hand didn't make the matched set. The dry-run report counts
/// these — the coverage line that keeps us honest (design doc 05).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Skip {
    /// Tournament or non-NLHE game.
    NotNlheCash,
    /// Observed hand — no "Dealt to" hero.
    NoHero,
    /// The hand ended preflop.
    NoFlop,
    HeroNotOnFlop,
    /// Three or more players saw the flop.
    Multiway,
    /// Limped or 4-bet+ pot — no library formation models it.
    PotType,
    /// The (aggressor, caller) positions aren't a library formation.
    Formation,
    /// Effective flop stack outside the library depth band.
    Stack,
    /// Flop pot beyond ±25% of the formation's.
    Pot,
}

impl Skip {
    pub fn label(self) -> &'static str {
        match self {
            Skip::NotNlheCash => "tournament / non-NLHE",
            Skip::NoHero => "no hero cards (observed hand)",
            Skip::NoFlop => "no flop",
            Skip::HeroNotOnFlop => "hero folded preflop",
            Skip::Multiway => "multiway flop",
            Skip::PotType => "limped or 4-bet+ pot",
            Skip::Formation => "uncovered formation",
            Skip::Stack => "stack outside depth band",
            Skip::Pot => "pot mismatch > 25%",
        }
    }
}

/// A hand the library models, carrying everything the scoring walk needs:
/// the formation, the board, hero's cards, and the postflop line in bb.
pub struct Matched {
    pub hand_id: String,
    pub formation: &'static Formation,
    /// Packed lowercase flop as dealt, e.g. `"td9d6h"` (grouping/report key).
    pub flop: String,
    /// All dealt cards in HH case, e.g. `["Td","9d","6h","2c","7s"]`.
    pub board: Vec<String>,
    /// Hero's holding packed, e.g. `"AsKh"`.
    pub hero_cards: String,
    pub hero_oop: bool,
    /// Postflop actions per street as `(is_hero, act)`, amounts in bb.
    pub streets: [Vec<(bool, Act)>; 3],
    /// Hero's postflop action count — what the scoring pass grades.
    pub decisions: usize,
    /// Pot at the flop, bb.
    pub pot_bb: f32,
    /// Effective stack behind at the flop, bb.
    pub stack_bb: f32,
}

/// Split a file into hands and parse each; returns parsed hands plus the
/// count of blocks that didn't parse. Unrecognized text is ignored.
/// GGPoker exports (`Poker Hand #…`) mirror the PokerStars format line for
/// line, so one parser reads both (design 05 milestone 3).
pub fn parse_file(text: &str) -> (Vec<RawHand>, usize) {
    let mut blocks: Vec<Vec<&str>> = Vec::new();
    for line in text.lines().map(str::trim_end) {
        if line.starts_with("PokerStars ") || line.starts_with("Poker Hand #") {
            blocks.push(vec![line]);
        } else if let Some(block) = blocks.last_mut() {
            if !line.is_empty() {
                block.push(line);
            }
        }
    }
    let mut hands = Vec::new();
    let mut failed = 0;
    for block in blocks {
        match parse_hand(&block) {
            Ok(h) => hands.push(h),
            Err(_) => failed += 1,
        }
    }
    (hands, failed)
}

fn parse_hand(lines: &[&str]) -> Result<RawHand, String> {
    let header = lines.first().ok_or("empty hand")?;
    let mut hand = RawHand {
        id: header
            .split('#')
            .nth(1)
            .and_then(|s| s.split(':').next())
            .unwrap_or_default()
            .into(),
        nlhe_cash: header.contains("Hold'em No Limit") && !header.contains("Tournament #"),
        // First parenthesized "sb/bb" group; tournaments' "(30/60)" works too.
        bb: header
            .split('(')
            .filter_map(|p| p.split(')').next())
            .find(|p| p.contains('/'))
            .and_then(|p| parse_amount(p.split('/').nth(1)?))
            .ok_or("no stakes in header")?,
        ..Default::default()
    };

    let mut street = 0usize; // 0 = preflop, 1..=3 = flop/turn/river
    for line in &lines[1..] {
        if let Some(rest) = line.strip_prefix("Table '") {
            hand.button_seat = rest
                .rsplit("Seat #")
                .next()
                .and_then(|s| s.split_whitespace().next()?.parse().ok())
                .unwrap_or(0);
        } else if line.starts_with("*** ") {
            if line.starts_with("*** FLOP") {
                hand.board = last_bracket(line);
                street = 1;
            } else if line.starts_with("*** TURN") {
                hand.board.extend(last_bracket(line));
                street = 2;
            } else if line.starts_with("*** RIVER") {
                hand.board.extend(last_bracket(line));
                street = 3;
            } else if line.starts_with("*** SHOW") || line.starts_with("*** SUMMARY") {
                // "SHOW DOWN" (PokerStars) or "SHOWDOWN" (GGPoker): board +
                // actions are complete; the summary re-lists seats.
                break;
            } // *** HOLE CARDS *** stays street 0
        } else if street == 0 && line.starts_with("Seat ") {
            if let Some(seat) = parse_seat(&line["Seat ".len()..]) {
                hand.seats.push(seat);
            }
        } else if let Some(rest) = line.strip_prefix("Dealt to ") {
            if let Some(open) = rest.rfind('[') {
                hand.hero = Some(rest[..open].trim().into());
                hand.hero_cards = last_bracket(rest);
            }
        } else if let Some((player, verb)) = split_player(line, &hand.seats) {
            if verb.starts_with("posts small blind") {
                hand.sb_player = Some(player.clone());
            } else if verb.starts_with("posts big blind") {
                hand.bb_player = Some(player.clone());
            }
            if let Some(act) = parse_act(verb) {
                let action = Action {
                    player: player.clone(),
                    act,
                };
                match street {
                    0 => hand.preflop.push(action),
                    s => hand.postflop[s - 1].push(action),
                }
            }
        } // chat, uncalled-bet, collected, shows, … — ignored
    }
    if hand.seats.is_empty() {
        return Err("no seats".into());
    }
    Ok(hand)
}

/// `N: name ($stack in chips)[ is sitting out]` — the tail after `"Seat "`.
fn parse_seat(rest: &str) -> Option<(u32, String, f32)> {
    let (no, rest) = rest.split_once(": ")?;
    let chips = rest.find(" in chips")?;
    let open = rest[..chips].rfind('(')?;
    Some((
        no.parse().ok()?,
        rest[..open].trim().to_string(),
        parse_amount(&rest[open + 1..chips])?,
    ))
}

/// `<seat name>: <verb …>` → the roster owner and the verb. Longest matching
/// name wins so a name that prefixes another can't steal its lines.
fn split_player<'a>(
    line: &'a str,
    seats: &'a [(u32, String, f32)],
) -> Option<(&'a String, &'a str)> {
    seats
        .iter()
        .filter_map(|(_, n, _)| Some((n, line.strip_prefix(n.as_str())?.strip_prefix(": ")?)))
        .max_by_key(|(n, _)| n.len())
}

/// Parse an action verb. Unknown verbs (shows, mucks, timeouts, …) are
/// `None` — the analyzer only consumes money actions.
fn parse_act(verb: &str) -> Option<Act> {
    let verb = verb.strip_suffix(" and is all-in").unwrap_or(verb);
    if verb.starts_with("folds") {
        Some(Act::Fold)
    } else if verb == "checks" {
        Some(Act::Check)
    } else if let Some(amt) = verb.strip_prefix("calls ") {
        Some(Act::Call(parse_amount(amt)?))
    } else if let Some(rest) = verb.strip_prefix("raises ") {
        Some(Act::RaiseTo(parse_amount(rest.split(" to ").nth(1)?)?))
    } else if let Some(amt) = verb.strip_prefix("bets ") {
        Some(Act::Bet(parse_amount(amt)?))
    } else if verb.starts_with("posts ") {
        // "small blind $0.50" / "big blind $1" / "small & big blinds $1.50" /
        // "the ante $0.10" — the amount is always the last token.
        Some(Act::Post(parse_amount(verb.rsplit(' ').next()?)?))
    } else {
        None
    }
}

/// Leading float after any currency symbol: `"$1.50"` → 1.5, `"1.00 USD"` →
/// 1.0. PokerStars exports don't use thousands separators.
fn parse_amount(s: &str) -> Option<f32> {
    let s = s.trim_start_matches(|c: char| !c.is_ascii_digit());
    let end = s
        .find(|c: char| !c.is_ascii_digit() && c != '.')
        .unwrap_or(s.len());
    s[..end].parse().ok()
}

/// Cards inside the last `[…]` group of a line.
fn last_bracket(line: &str) -> Vec<String> {
    let Some(open) = line.rfind('[') else {
        return Vec::new();
    };
    line[open + 1..]
        .split(']')
        .next()
        .unwrap_or("")
        .split_whitespace()
        .map(str::to_string)
        .collect()
}

/// Players dealt in — anyone who posted or acted preflop, in seat order.
/// Sitting-out seats never act, so they fall out here.
fn participants(h: &RawHand) -> Vec<&str> {
    h.seats
        .iter()
        .map(|(_, n, _)| n.as_str())
        .filter(|n| h.preflop.iter().any(|a| a.player == *n))
        .collect()
}

/// Map participants to position names. Blinds come from their post lines,
/// BTN from the header's button seat, and seats counting back from the
/// button get CO/HJ/…; only BTN/CO/SB/BB can match a formation, so early-
/// position precision is moot.
fn positions(h: &RawHand) -> BTreeMap<&str, &'static str> {
    let mut pos = BTreeMap::new();
    for (player, label) in [(&h.sb_player, "SB"), (&h.bb_player, "BB")] {
        if let Some(n) = player {
            pos.insert(n.as_str(), label);
        }
    }
    let ring: Vec<(u32, &str)> = h
        .seats
        .iter()
        .filter(|(_, n, _)| h.preflop.iter().any(|a| &a.player == n))
        .map(|(s, n, _)| (*s, n.as_str()))
        .collect();
    let Some(btn) = ring.iter().position(|(s, _)| *s == h.button_seat) else {
        return pos; // dead button: non-blind seats stay unlabeled
    };
    // Walk backwards from the button. In heads-up the button already wears
    // "SB", so the BTN label is (correctly) never handed out.
    let mut labels = ["BTN", "CO", "HJ", "MP", "UTG"].iter();
    for k in 0..ring.len() {
        let name = ring[(btn + ring.len() - k) % ring.len()].1;
        if !pos.contains_key(name) {
            pos.insert(name, labels.next().copied().unwrap_or("EP"));
        }
    }
    pos
}

/// Per-player chips put in during one street. `RaiseTo` sets the street
/// total; everything else adds — PokerStars' printing convention.
fn street_totals(actions: &[Action]) -> BTreeMap<&str, f32> {
    let mut paid: BTreeMap<&str, f32> = BTreeMap::new();
    for a in actions {
        let e = paid.entry(a.player.as_str()).or_insert(0.0);
        match a.act {
            Act::Call(x) | Act::Bet(x) | Act::Post(x) => *e += x,
            Act::RaiseTo(to) => *e = to,
            Act::Fold | Act::Check => {}
        }
    }
    paid
}

/// Match one parsed hand against the library: heads-up by the flop, in a
/// covered formation, within the depth band and pot tolerance (design 05).
pub fn normalize(h: &RawHand) -> Result<Matched, Skip> {
    if !h.nlhe_cash || h.bb <= 0.0 {
        return Err(Skip::NotNlheCash);
    }
    let hero = match h.hero.as_deref() {
        Some(n) if h.hero_cards.len() == 2 => n,
        _ => return Err(Skip::NoHero),
    };
    if h.board.len() < 3 {
        return Err(Skip::NoFlop);
    }
    let on_flop: Vec<&str> = participants(h)
        .into_iter()
        .filter(|p| {
            !h.preflop
                .iter()
                .any(|a| a.player == *p && a.act == Act::Fold)
        })
        .collect();
    if !on_flop.contains(&hero) {
        return Err(Skip::HeroNotOnFlop);
    }
    if on_flop.len() != 2 {
        return Err(Skip::Multiway);
    }

    let raisers: Vec<&str> = h
        .preflop
        .iter()
        .filter(|a| matches!(a.act, Act::RaiseTo(_)))
        .map(|a| a.player.as_str())
        .collect();
    let prefix = match raisers.len() {
        1 => "srp",
        2 => "3bp",
        _ => return Err(Skip::PotType),
    };
    let aggressor = *raisers.last().expect("counted above");
    if !on_flop.contains(&aggressor) {
        return Err(Skip::Formation); // e.g. a squeeze where the opener folded
    }
    let caller = *on_flop
        .iter()
        .find(|p| **p != aggressor)
        .expect("two on flop");

    let pos = positions(h);
    let p = |name: &str| pos.get(name).copied().unwrap_or("?");
    let id = format!(
        "{prefix}-{}-{}",
        p(aggressor).to_lowercase(),
        p(caller).to_lowercase()
    );
    let f = formation(&id).ok_or(Skip::Formation)?;

    let paid = street_totals(&h.preflop);
    let start = |name: &str| {
        h.seats
            .iter()
            .find(|(_, n, _)| n == name)
            .map(|(_, _, s)| *s)
            .unwrap_or(0.0)
    };
    let stack_bb = on_flop
        .iter()
        .map(|n| (start(n) - paid.get(n).copied().unwrap_or(0.0)) / h.bb)
        .fold(f32::INFINITY, f32::min);
    // ponytail: one depth per formation today — a band around it stands in
    // for "nearest library depth" until depth variants exist (design 02).
    if !(STACK_BAND.0 * f.stack_bb..=STACK_BAND.1 * f.stack_bb).contains(&stack_bb) {
        return Err(Skip::Stack);
    }
    let pot_bb = paid.values().sum::<f32>() / h.bb;
    if (pot_bb - f.pot_bb).abs() > POT_TOLERANCE * f.pot_bb {
        return Err(Skip::Pot);
    }

    let streets = h.postflop.each_ref().map(|acts| {
        acts.iter()
            .map(|a| (a.player == hero, to_bb(&a.act, h.bb)))
            .collect::<Vec<_>>()
    });
    Ok(Matched {
        hand_id: h.id.clone(),
        formation: f,
        flop: h.board[..3].join("").to_lowercase(),
        board: h.board.clone(),
        hero_cards: h.hero_cards.join(""),
        hero_oop: p(hero) == f.oop_seat,
        decisions: streets.iter().flatten().filter(|(hero, _)| *hero).count(),
        streets,
        pot_bb,
        stack_bb,
    })
}

/// The same action with its amount converted from table currency to bb.
fn to_bb(act: &Act, bb: f32) -> Act {
    match *act {
        Act::Call(x) => Act::Call(x / bb),
        Act::Bet(x) => Act::Bet(x / bb),
        Act::RaiseTo(x) => Act::RaiseTo(x / bb),
        Act::Post(x) => Act::Post(x / bb),
        Act::Fold => Act::Fold,
        Act::Check => Act::Check,
    }
}

/// Order-independent flop identity (same trick as `solution::flop_key`).
fn sorted_flop(flop: &str) -> String {
    let mut cards: Vec<&str> = flop
        .as_bytes()
        .chunks(2)
        .map(|c| std::str::from_utf8(c).unwrap_or(""))
        .collect();
    cards.sort_unstable();
    cards.concat()
}

/// `"10m"`, `"45s"`, `"1h"`, or plain seconds; `0` disables solving.
pub fn parse_budget(s: &str) -> Option<Duration> {
    let s = s.trim();
    let (num, mult) = match s.as_bytes().last()? {
        b'h' => (&s[..s.len() - 1], 3600.0),
        b'm' => (&s[..s.len() - 1], 60.0),
        b's' => (&s[..s.len() - 1], 1.0),
        _ => (s, 1.0),
    };
    let v: f64 = num.trim().parse().ok()?;
    (v >= 0.0).then(|| Duration::from_secs_f64(v * mult))
}

/// Map a hand action onto a tree node's action labels — exact for
/// fold/check/call, nearest size by log-ratio for bets and raises (standard
/// solver-analysis practice, noted in the report footer). `None` when the
/// tree doesn't offer the move at all (e.g. a donk lead): off-tree.
fn map_act(act: &Act, labels: &[String]) -> Option<usize> {
    match act {
        Act::Fold => labels.iter().position(|l| l == "Fold"),
        Act::Check => labels.iter().position(|l| l == "Check"),
        Act::Call(_) => labels.iter().position(|l| l == "Call"),
        Act::Bet(x) => nearest(labels, &["Bet ", "All-in "], *x),
        Act::RaiseTo(x) => nearest(labels, &["Raise to ", "All-in "], *x),
        Act::Post(_) => None,
    }
}

/// Among labels shaped `"<prefix><amount>bb"`, the index nearest `x` by ratio.
fn nearest(labels: &[String], prefixes: &[&str], x: f32) -> Option<usize> {
    labels
        .iter()
        .enumerate()
        .filter_map(|(i, l)| {
            let amt: f32 = prefixes
                .iter()
                .find_map(|p| l.strip_prefix(p))?
                .strip_suffix("bb")?
                .parse()
                .ok()?;
            Some((i, (x.max(1e-9) / amt.max(1e-9)).ln().abs()))
        })
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(i, _)| i)
}

/// The hero combo's strategy at a player node; `None` when his actual holding
/// isn't in the library range for that seat.
fn hero_strategy(node: &TreeNode, hero: &str) -> Option<crate::solution::NodeStrategy> {
    let rev = format!("{}{}", hero.get(2..4)?, hero.get(..2)?);
    let j = node.hands.iter().position(|h| h == hero || *h == rev)?;
    Some(crate::solution::NodeStrategy {
        actions: node.actions.clone(),
        frequencies: node.freqs.iter().map(|f| f[j]).collect(),
        action_ev: node.evs.iter().map(|e| e[j]).collect(),
    })
}

/// Why a matched hand's remaining decisions went unscored mid-walk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Lost {
    /// The real line left the solved tree (donk lead, extra raise, truncated
    /// record) — everything from there is unscored, never guessed.
    OffTree,
    /// Hero's actual holding isn't in the library range for his seat.
    OutOfRange,
}

/// Replay one matched hand through the solved tree, scoring each hero
/// decision. Returns the records plus why the rest went unscored (if any);
/// `Err` means the session itself died.
fn walk_hand(
    session: &mut TreeSession,
    m: &Matched,
) -> io::Result<(Vec<StatRecord>, Option<Lost>)> {
    let hero_seat = if m.hero_oop { "oop" } else { "ip" };
    let mut node = session.root()?;
    let mut streets = m.streets.each_ref().map(|v| v.iter());
    let mut recs = Vec::new();
    let lost = loop {
        match node.player.as_str() {
            "terminal" => break (recs.len() < m.decisions).then_some(Lost::OffTree),
            "chance" => match m.board.get(node.board.len()) {
                Some(card) => node = session.deal(card)?,
                None => break (recs.len() < m.decisions).then_some(Lost::OffTree),
            },
            seat => {
                let street = node.board.len().saturating_sub(3).min(2);
                let Some((is_hero, act)) = streets[street].next() else {
                    break (recs.len() < m.decisions).then_some(Lost::OffTree);
                };
                if *is_hero != (seat == hero_seat) {
                    break Some(Lost::OffTree); // desynced from the tree
                }
                let Some(chosen) = map_act(act, &node.actions) else {
                    break Some(Lost::OffTree);
                };
                if *is_hero {
                    let Some(ns) = hero_strategy(&node, &m.hero_cards) else {
                        break Some(Lost::OutOfRange);
                    };
                    recs.push(decision_record(m, &node, &ns, chosen));
                }
                node = session.play(chosen)?;
            }
        }
    };
    Ok((recs, lost))
}

fn decision_record(
    m: &Matched,
    node: &TreeNode,
    ns: &crate::solution::NodeStrategy,
    chosen: usize,
) -> StatRecord {
    let (texture, bucket) = crate::trainer::flop_context(&node.board, &m.hero_cards);
    StatRecord {
        hand_id: m.hand_id.clone(),
        formation: m.formation.id.into(),
        flop: m.flop.clone(),
        texture,
        street: crate::trainer::street_name(node.board.len()).into(),
        hand: m.hero_cards.clone(),
        bucket,
        line: node.line.clone(),
        chosen: ns.actions[chosen].clone(),
        best: ns.actions[ns.best()].clone(),
        ev_loss: Some(ns.ev_loss(chosen)),
        gto_freq: Some(ns.frequencies[chosen]),
        ..StatRecord::new("analyze")
    }
}

/// What scoring produced; all counts are hero decisions.
#[derive(Default)]
struct Outcome {
    records: Vec<StatRecord>,
    /// In groups never solved because the budget ran out.
    budget_skipped: usize,
    /// In groups whose solve (or session) failed.
    solve_failed: usize,
    off_tree: usize,
    out_of_range: usize,
}

/// Solve each distinct (formation, flop) group — most hands first, until the
/// wall-clock budget is spent — and walk its hands. One serve process is
/// reused across groups; a died session respawns on the next group.
fn score(matched: &[Matched], budget: Duration) -> Outcome {
    let mut groups: BTreeMap<(&str, String), Vec<&Matched>> = BTreeMap::new();
    for m in matched {
        groups
            .entry((m.formation.id, sorted_flop(&m.flop)))
            .or_default()
            .push(m);
    }
    let mut groups: Vec<_> = groups.into_iter().collect();
    groups.sort_by(|a, b| b.1.len().cmp(&a.1.len()).then(a.0.cmp(&b.0)));

    let mut out = Outcome::default();
    let mut session: Option<TreeSession> = None;
    let started = Instant::now();
    let total = groups.len();

    for (i, ((formation_id, _), hands)) in groups.into_iter().enumerate() {
        let decisions = |ms: &[&Matched]| ms.iter().map(|m| m.decisions).sum::<usize>();
        if started.elapsed() >= budget {
            out.budget_skipped += decisions(&hands);
            continue;
        }
        let flop = hands[0].board[..3].join("");
        let config = match SpotConfig::for_formation(formation_id, "data/ranges") {
            Ok(c) => c,
            Err(e) => {
                eprintln!("skipping {formation_id} {flop}: {e}");
                out.solve_failed += decisions(&hands);
                continue;
            }
        };
        eprintln!(
            "[{}/{total}] solving {formation_id} {flop} ({} hand(s))…",
            i + 1,
            hands.len()
        );
        let req = SolveRequest { flop, config };
        let solved = match session.as_mut() {
            Some(s) => s.solve(&req).map(|_| ()),
            None => TreeSession::start(&req).map(|(s, _)| session = Some(s)),
        };
        if let Err(e) = solved {
            eprintln!("  solve failed: {e}");
            session = None;
            out.solve_failed += decisions(&hands);
            continue;
        }
        let s = session.as_mut().expect("solved above");
        for (j, m) in hands.iter().enumerate() {
            match walk_hand(s, m) {
                Ok((recs, lost)) => {
                    let missing = m.decisions - recs.len();
                    match lost {
                        Some(Lost::OutOfRange) => out.out_of_range += missing,
                        _ => out.off_tree += missing,
                    }
                    out.records.extend(recs);
                }
                Err(e) => {
                    eprintln!("  session died: {e}");
                    session = None;
                    out.solve_failed += decisions(&hands[j..]);
                    break;
                }
            }
        }
    }
    out
}

/// The scoring half of the report: coverage, leak tables, blunder list.
fn print_score_report(out: &Outcome, matched_decisions: usize, skipped_decisions: usize) {
    let total = matched_decisions + skipped_decisions;
    let scored = out.records.len();
    let pct = |n: usize| 100.0 * n as f64 / total.max(1) as f64;
    println!(
        "\nCoverage: scored {scored} of {total} hero decision(s) ({:.0}%).",
        pct(scored)
    );
    for (label, n) in [
        ("unscored — solve budget", out.budget_skipped),
        ("unscored — solve failed", out.solve_failed),
        ("unscored — off-tree line", out.off_tree),
        ("unscored — hand outside library range", out.out_of_range),
        ("skipped — unmatched hands", skipped_decisions),
    ] {
        if n > 0 {
            println!("  {label:<38} {n:>5} ({:.0}%)", pct(n));
        }
    }
    if scored == 0 {
        return;
    }

    let refs: Vec<&StatRecord> = out.records.iter().collect();
    let all = stats::summarize("all", &refs);
    println!(
        "\n{scored} scored decision(s): avg EV loss {:.3}bb ({}) | accuracy {:.0}% | blunders {}",
        all.avg_ev_loss,
        stats::band(all.avg_ev_loss),
        100.0 * all.accuracy,
        all.blunders,
    );
    for by in [
        GroupBy::Street,
        GroupBy::Texture,
        GroupBy::Bucket,
        GroupBy::Formation,
    ] {
        println!();
        stats::print_by(&out.records, by);
    }

    let mut blunders: Vec<&StatRecord> = out
        .records
        .iter()
        .filter(|r| r.ev_loss.is_some_and(|e| e > stats::BLUNDER_BB))
        .collect();
    blunders.sort_by(|a, b| b.ev_loss.unwrap().total_cmp(&a.ev_loss.unwrap()));
    if !blunders.is_empty() {
        println!("\nTop blunders (> {}bb lost):", stats::BLUNDER_BB);
        for r in blunders.iter().take(10) {
            println!(
                "  -{:.2}bb  #{}  {} {}  {}: chose {} (best {}, GTO plays yours {:.0}%)",
                r.ev_loss.unwrap(),
                r.hand_id,
                r.flop,
                r.hand,
                r.street,
                r.chosen,
                r.best,
                100.0 * r.gto_freq.unwrap_or(0.0),
            );
            let line = if r.line.is_empty() {
                String::new()
            } else {
                format!(" --line '{}'", r.line.join(","))
            };
            println!(
                "           replay: poker-trainer table --board {} --formation {}{line}",
                r.flop, r.formation
            );
        }
    }
}

/// Dump scored records as JSONL for external tooling (design 05 milestone 4).
fn write_jsonl(path: &Path, records: &[StatRecord]) -> io::Result<()> {
    let mut f = fs::File::create(path)?;
    for r in records {
        writeln!(f, "{}", serde_json::to_string(r)?)?;
    }
    Ok(())
}

/// Normalize every parsed hand: the matched set, skip counts by reason, and
/// how many hero postflop decisions the skipped hands held (the honest
/// denominator for the coverage line).
fn match_hands(hands: &[RawHand]) -> (Vec<Matched>, BTreeMap<&'static str, usize>, usize) {
    let mut matched = Vec::new();
    let mut skips: BTreeMap<&'static str, usize> = BTreeMap::new();
    let mut skipped_decisions = 0;
    for h in hands {
        match normalize(h) {
            Ok(m) => matched.push(m),
            Err(s) => {
                *skips.entry(s.label()).or_default() += 1;
                if let Some(hero) = &h.hero {
                    skipped_decisions += h
                        .postflop
                        .iter()
                        .flatten()
                        .filter(|a| &a.player == hero)
                        .count();
                }
            }
        }
    }
    (matched, skips, skipped_decisions)
}

/// The match/coverage report (design 05 milestone 1; also the header of the
/// scored report).
fn print_report(
    hands: &[RawHand],
    unparsed: usize,
    matched: &[Matched],
    skips: &BTreeMap<&'static str, usize>,
) {
    print!("Parsed {} hand(s)", hands.len());
    if unparsed > 0 {
        print!(" ({unparsed} unparseable block(s) skipped)");
    }
    println!(".");

    let decisions: usize = matched.iter().map(|m| m.decisions).sum();
    println!(
        "\nMatched {} hand(s) ({:.0}%) — {decisions} hero postflop decision(s):",
        matched.len(),
        100.0 * matched.len() as f64 / hands.len().max(1) as f64,
    );
    let mut by_formation: BTreeMap<&str, (usize, usize)> = BTreeMap::new();
    for m in matched {
        let e = by_formation.entry(m.formation.id).or_default();
        e.0 += 1;
        e.1 += m.decisions;
    }
    for (id, (n, d)) in &by_formation {
        println!("  {id:<12} {n:>5} hands  {d:>5} decisions");
    }
    if matched.is_empty() {
        println!("  (none)");
    }

    if !skips.is_empty() {
        println!("\nSkipped {} hand(s):", skips.values().sum::<usize>());
        let mut rows: Vec<_> = skips.iter().collect();
        rows.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
        for (label, n) in rows {
            println!("  {label:<30} {n:>5}");
        }
    }

    let gap: BTreeSet<(&str, String)> = matched
        .iter()
        .map(|m| (m.formation.id, sorted_flop(&m.flop)))
        .collect();
    println!(
        "\nLibrary gap: {} distinct (formation, flop) spot(s) to solve for full scoring.",
        gap.len()
    );
}

/// Entry point for `poker-trainer analyze`.
pub fn run(files: &[PathBuf], dry_run: bool, solve_budget: &str, jsonl: Option<&Path>) {
    let Some(budget) = parse_budget(solve_budget) else {
        eprintln!("bad --solve-budget {solve_budget:?} (try \"10m\", \"45s\", or seconds)");
        return;
    };
    let mut hands = Vec::new();
    let mut unparsed = 0;
    for path in files {
        match fs::read_to_string(path) {
            Ok(text) => {
                let (h, u) = parse_file(&text);
                hands.extend(h);
                unparsed += u;
            }
            Err(e) => eprintln!("skipping {}: {e}", path.display()),
        }
    }
    if hands.is_empty() && unparsed == 0 {
        println!("No PokerStars/GGPoker hands found in the given file(s).");
        return;
    }
    let (matched, skips, skipped_decisions) = match_hands(&hands);
    print_report(&hands, unparsed, &matched, &skips);

    if !dry_run && !matched.is_empty() {
        let outcome = score(&matched, budget);
        let matched_decisions = matched.iter().map(|m| m.decisions).sum();
        print_score_report(&outcome, matched_decisions, skipped_decisions);
        if let Some(path) = jsonl {
            match write_jsonl(path, &outcome.records) {
                Ok(()) => println!(
                    "\nWrote {} record(s) to {}.",
                    outcome.records.len(),
                    path.display()
                ),
                Err(e) => eprintln!("couldn't write {}: {e}", path.display()),
            }
        }
    } else if let Some(path) = jsonl {
        eprintln!("(--jsonl skipped: nothing scored — {})", path.display());
    }

    println!("\nMatching buckets stacks/pots to the nearest library config and maps bet");
    println!("sizes to the tree's nearest size; EV loss measures distance from the");
    println!("library's equilibrium, not from your opponents' actual ranges.");
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Synthetic hands only — never commit real histories (design doc 05).
    const GOLDEN: &str = include_str!("../tests/fixtures/pokerstars.txt");

    #[test]
    fn golden_parses_hand_fields() {
        let (hands, _) = parse_file(GOLDEN);
        let h = &hands[0];
        assert_eq!(h.id, "1001");
        assert!(h.nlhe_cash);
        assert!((h.bb - 1.0).abs() < 1e-6);
        assert_eq!(h.button_seat, 3);
        assert_eq!(h.seats.len(), 6);
        assert_eq!(h.seats[1].1, "hj player"); // name with a space
        assert_eq!(h.seats[5].1, "co (fish)"); // parens + sitting out
        assert!((h.seats[5].2 - 43.0).abs() < 1e-6);
        assert_eq!(h.hero.as_deref(), Some("Hero"));
        assert_eq!(h.hero_cards, vec!["As", "Kh"]);
        assert_eq!(h.board, vec!["Td", "9d", "6h", "2c", "7s"]);
        assert_eq!(h.sb_player.as_deref(), Some("sb_villain"));
        assert_eq!(h.bb_player.as_deref(), Some("bb_villain"));
        assert_eq!(h.preflop.len(), 7); // 2 posts + 5 actions
        assert_eq!(h.postflop[0].len(), 3);
        assert_eq!(h.postflop[2].last().unwrap().act, Act::Fold);
    }

    #[test]
    fn golden_matches_and_skips_every_hand() {
        let (hands, failed) = parse_file(GOLDEN);
        assert_eq!((hands.len(), failed), (11, 1));

        let m = normalize(&hands[0]).unwrap();
        assert_eq!(m.formation.id, "srp-btn-bb");
        assert_eq!(m.flop, "td9d6h");
        assert!(!m.hero_oop);
        assert_eq!(m.decisions, 3);
        assert!((m.pot_bb - 5.5).abs() < 1e-4);
        assert!((m.stack_bb - 97.5).abs() < 1e-4);
        // The walkable line: HH case kept, amounts in bb, hero flagged.
        assert_eq!(m.hand_id, "1001");
        assert_eq!(m.hero_cards, "AsKh");
        assert_eq!(m.board, vec!["Td", "9d", "6h", "2c", "7s"]);
        assert_eq!(
            m.streets[0],
            vec![
                (false, Act::Check),
                (true, Act::Bet(1.75)),
                (false, Act::Call(1.75)),
            ]
        );
        assert_eq!(
            m.streets[2],
            vec![(false, Act::Bet(4.5)), (true, Act::Fold)]
        );

        let m = normalize(&hands[1]).unwrap();
        assert_eq!(m.formation.id, "3bp-bb-btn");
        assert!(m.hero_oop);
        assert_eq!(m.decisions, 1);

        use Skip::*;
        let expect = [
            Multiway,
            PotType,
            NotNlheCash,
            NoHero,
            NoFlop,
            HeroNotOnFlop,
            Stack,
            Formation,
            Pot,
        ];
        for (h, want) in hands[2..].iter().zip(expect) {
            assert_eq!(normalize(h).err(), Some(want), "hand #{}", h.id);
        }
    }

    /// Synthetic GGPoker hands — same rules as the PS fixture.
    const GG: &str = include_str!("../tests/fixtures/ggpoker.txt");

    #[test]
    fn ggpoker_golden_parses_matches_and_skips() {
        let (hands, failed) = parse_file(GG);
        assert_eq!((hands.len(), failed), (3, 0));

        let m = normalize(&hands[0]).unwrap();
        assert_eq!(m.hand_id, "HD2001");
        assert_eq!(m.formation.id, "srp-btn-bb");
        assert_eq!(m.hero_cards, "AhKd");
        assert!(!m.hero_oop);
        assert_eq!(m.decisions, 1);
        assert_eq!(m.streets[0][1], (true, Act::Bet(2.0)));

        assert_eq!(normalize(&hands[1]).err(), Some(Skip::NotNlheCash));

        // "*** SHOWDOWN ***" (GG spelling) ends the action like PS's version.
        let m = normalize(&hands[2]).unwrap();
        assert_eq!(m.decisions, 3);
        assert_eq!(m.board.len(), 5);
        assert_eq!(m.streets[2], vec![(false, Act::Check), (true, Act::Check)]);
    }

    #[test]
    fn budget_parses_units_and_rejects_garbage() {
        assert_eq!(parse_budget("10m"), Some(Duration::from_secs(600)));
        assert_eq!(parse_budget("45s"), Some(Duration::from_secs(45)));
        assert_eq!(parse_budget("1h"), Some(Duration::from_secs(3600)));
        assert_eq!(parse_budget("90"), Some(Duration::from_secs(90)));
        assert_eq!(parse_budget("0"), Some(Duration::ZERO));
        assert_eq!(parse_budget("-5m"), None);
        assert_eq!(parse_budget("soon"), None);
    }

    #[test]
    fn actions_map_to_nearest_tree_size() {
        let bet: Vec<String> = ["Fold", "Check", "Bet 2.0bb", "Bet 4.5bb", "All-in 97.0bb"]
            .map(String::from)
            .to_vec();
        assert_eq!(map_act(&Act::Check, &bet), Some(1));
        assert_eq!(map_act(&Act::Fold, &bet), Some(0));
        assert_eq!(map_act(&Act::Bet(2.2), &bet), Some(2));
        assert_eq!(map_act(&Act::Bet(3.5), &bet), Some(3));
        // Overbet jam: nearer the all-in than the biggest bet, by ratio.
        assert_eq!(map_act(&Act::Bet(50.0), &bet), Some(4));
        assert_eq!(map_act(&Act::Call(3.0), &bet), None); // nothing to call

        let raise: Vec<String> = ["Fold", "Call", "Raise to 7.5bb", "All-in 97.0bb"]
            .map(String::from)
            .to_vec();
        assert_eq!(map_act(&Act::Call(2.0), &raise), Some(1));
        assert_eq!(map_act(&Act::RaiseTo(8.0), &raise), Some(2));
        assert_eq!(map_act(&Act::RaiseTo(80.0), &raise), Some(3));
        assert_eq!(map_act(&Act::Post(1.0), &raise), None);

        // A donk lead at a node whose tree offers no bet: off-tree, not guessed.
        let check_only = vec!["Check".to_string()];
        assert_eq!(map_act(&Act::Bet(3.0), &check_only), None);
    }

    #[test]
    fn hero_strategy_matches_either_card_order() {
        let node = TreeNode {
            player: "ip".into(),
            actions: vec!["Check".into(), "Bet 2.0bb".into()],
            hands: vec!["AsKs".into(), "KhAh".into()],
            freqs: vec![vec![0.2, 0.4], vec![0.8, 0.6]],
            evs: vec![vec![1.0, 1.1], vec![3.5, 3.6]],
            ..Default::default()
        };
        let ns = hero_strategy(&node, "AhKh").unwrap(); // stored reversed
        assert_eq!(ns.frequencies, vec![0.4, 0.6]);
        assert_eq!(ns.action_ev, vec![1.1, 3.6]);
        assert!(hero_strategy(&node, "QdQc").is_none()); // not in range
    }

    /// End-to-end: solve a tiny spot and replay a synthetic matched hand
    /// through it. Spawns cargo + a real solve: `cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn scoring_walks_a_tiny_solve() {
        let req = SolveRequest {
            flop: "Td9d6h".into(),
            config: SpotConfig {
                formation: "srp-btn-bb".into(),
                oop_range: "AA,KK".into(),
                ip_range: "QQ,JJ".into(),
                flop_sizes: "50%".into(),
                turn_sizes: "33%".into(),
                river_sizes: "33%".into(),
                stack_bb: 97.0,
                pot_bb: 6.0,
                rake_rate: 0.0,
                rake_cap_bb: 0.0,
            },
        };
        let m = Matched {
            hand_id: "t1".into(),
            formation: formation("srp-btn-bb").unwrap(),
            flop: "td9d6h".into(),
            board: ["Td", "9d", "6h", "2c"].map(String::from).to_vec(),
            hero_cards: "QsQc".into(), // in the IP range
            hero_oop: false,
            streets: [
                vec![
                    (false, Act::Check),
                    (true, Act::Bet(3.3)), // maps to the 50% size
                    (false, Act::Call(3.3)),
                ],
                vec![(false, Act::Check), (true, Act::Check)],
                vec![], // record ends at the turn — nothing left to score
            ],
            decisions: 2,
            pot_bb: 6.0,
            stack_bb: 97.0,
        };
        let (mut session, _root) = TreeSession::start(&req).unwrap();
        let (recs, lost) = walk_hand(&mut session, &m).unwrap();
        assert_eq!(lost, None); // all hero decisions scored
        assert_eq!(recs.len(), 2);
        assert_eq!(recs[0].street, "flop");
        assert_eq!(recs[0].drill, "analyze");
        assert_eq!(recs[0].hand_id, "t1");
        assert!(recs[0].chosen.starts_with("Bet"));
        assert_eq!(recs[1].street, "turn");
        assert_eq!(recs[1].line.last().unwrap(), "Check");
        assert!(recs
            .iter()
            .all(|r| r.ev_loss.unwrap() >= 0.0 && r.gto_freq.is_some()));

        // A hand whose hero holding the library range doesn't contain.
        let out = Matched {
            hero_cards: "7c2d".into(),
            streets: [
                vec![(false, Act::Check), (true, Act::Bet(3.3))],
                vec![],
                vec![],
            ],
            decisions: 1,
            ..m
        };
        let (recs, lost) = walk_hand(&mut session, &out).unwrap();
        assert!(recs.is_empty());
        assert_eq!(lost, Some(Lost::OutOfRange));

        // score() reuses one serve process across groups: a second op:solve
        // must replace the held game, not error.
        let mut req2 = req;
        req2.flop = "Ah7c2d".into();
        let root2 = session.solve(&req2).unwrap();
        assert_eq!(root2.board, vec!["2d", "7c", "Ah"]); // solver-sorted
        assert!(root2.line.is_empty());
    }

    #[test]
    fn amounts_parse_across_currencies() {
        assert_eq!(parse_amount("$1.50"), Some(1.5));
        assert_eq!(parse_amount("1.00 USD"), Some(1.0));
        assert_eq!(parse_amount("€0.55"), Some(0.55));
        assert_eq!(parse_amount("30"), Some(30.0));
        assert_eq!(parse_amount("garbage"), None);
    }

    #[test]
    fn verbs_parse_including_all_in_suffixes() {
        assert_eq!(
            parse_act("raises $4 to $6 and is all-in"),
            Some(Act::RaiseTo(6.0))
        );
        assert_eq!(parse_act("calls $20 and is all-in"), Some(Act::Call(20.0)));
        assert_eq!(
            parse_act("posts small & big blinds $1.50"),
            Some(Act::Post(1.5))
        );
        assert_eq!(parse_act("folds [Ah 2h]"), Some(Act::Fold));
        assert_eq!(parse_act("checks"), Some(Act::Check));
        assert_eq!(parse_act("shows [Ah Ad] (a pair of Aces)"), None);
        assert_eq!(parse_act("said, \"nice hand\""), None);
    }

    #[test]
    fn street_totals_raise_to_replaces_and_calls_add() {
        let a = |player: &str, act| Action {
            player: player.into(),
            act,
        };
        let actions = [
            a("sb", Act::Post(0.5)),
            a("bb", Act::Post(1.0)),
            a("btn", Act::RaiseTo(2.5)),
            a("sb", Act::Call(2.0)),
            a("bb", Act::RaiseTo(10.0)),
            a("btn", Act::Call(7.5)),
            a("sb", Act::Fold),
        ];
        let paid = street_totals(&actions);
        assert!((paid["sb"] - 2.5).abs() < 1e-6);
        assert!((paid["bb"] - 10.0).abs() < 1e-6);
        assert!((paid["btn"] - 10.0).abs() < 1e-6);
    }

    #[test]
    fn heads_up_button_is_the_small_blind() {
        let text = "PokerStars Hand #1: Hold'em No Limit ($0.50/$1.00) - x\n\
            Table 'T' 2-max Seat #1 is the button\n\
            Seat 1: a ($100 in chips)\n\
            Seat 2: b ($100 in chips)\n\
            a: posts small blind $0.50\n\
            b: posts big blind $1\n\
            *** HOLE CARDS ***\n\
            Dealt to a [As Ks]\n\
            a: raises $1.50 to $2.50\n\
            b: calls $1.50\n\
            *** FLOP *** [Ah 7h 2c]\n";
        let (hands, failed) = parse_file(text);
        assert_eq!(failed, 0);
        let pos = positions(&hands[0]);
        assert_eq!(pos["a"], "SB"); // never shadowed by a BTN label
        assert_eq!(pos["b"], "BB");
        // …so a HU open maps onto the blind-battle formation.
        let m = normalize(&hands[0]).unwrap();
        assert_eq!(m.formation.id, "srp-sb-bb");
        assert!(m.hero_oop);
        assert_eq!(m.decisions, 0);
    }
}
