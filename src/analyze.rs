//! Analyze: hand-history import & coverage report (design doc 05, P9).
//!
//! Pipeline: parse → normalize/match → score → report; each stage is a pure
//! function with its own tests. This is milestone 1 — the PokerStars text
//! parser, the formation matcher, and the `--dry-run` coverage report that
//! sizes the library gap. Nothing here touches a solver; scoring matched
//! hands through a `TreeSession` is milestone 2.

use crate::solution::{formation, Formation};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::PathBuf;

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

/// A hand the library models: the formation, the flop, and how much hero
/// played. Milestone 2 turns this into a `SpotConfig` + tree walk.
pub struct Matched {
    pub formation: &'static Formation,
    /// Packed lowercase flop as dealt, e.g. `"td9d6h"`.
    pub flop: String,
    pub hero_oop: bool,
    /// Hero's postflop action count — what a scoring pass would grade.
    pub decisions: usize,
    /// Pot at the flop, bb.
    pub pot_bb: f32,
    /// Effective stack behind at the flop, bb.
    pub stack_bb: f32,
}

/// Split a file into hands and parse each; returns parsed hands plus the
/// count of blocks that didn't parse. Non-PokerStars text is ignored.
pub fn parse_file(text: &str) -> (Vec<RawHand>, usize) {
    let mut blocks: Vec<Vec<&str>> = Vec::new();
    for line in text.lines().map(str::trim_end) {
        if line.starts_with("PokerStars ") {
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
            } else if line.starts_with("*** SHOW DOWN") || line.starts_with("*** SUMMARY") {
                break; // board + actions are complete; summary re-lists seats
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

    Ok(Matched {
        formation: f,
        flop: h.board[..3].join("").to_lowercase(),
        hero_oop: p(hero) == f.oop_seat,
        decisions: h
            .postflop
            .iter()
            .flatten()
            .filter(|a| a.player == hero)
            .count(),
        pot_bb,
        stack_bb,
    })
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

/// The dry-run coverage report (design 05 milestone 1).
fn print_report(hands: &[RawHand], unparsed: usize) {
    let mut matched: Vec<Matched> = Vec::new();
    let mut skips: BTreeMap<&'static str, usize> = BTreeMap::new();
    for h in hands {
        match normalize(h) {
            Ok(m) => matched.push(m),
            Err(s) => *skips.entry(s.label()).or_default() += 1,
        }
    }

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
    for m in &matched {
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
        let mut rows: Vec<_> = skips.into_iter().collect();
        rows.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
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
pub fn run(files: &[PathBuf], dry_run: bool) {
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
        println!("No PokerStars hands found in the given file(s).");
        return;
    }
    print_report(&hands, unparsed);
    println!("\nMatching buckets stacks/pots to the nearest library config; EV (once");
    println!("scored) measures distance from library equilibrium, not from your");
    println!("opponents' actual ranges.");
    if !dry_run {
        println!("(Scoring lands with P9 milestone 2 — the above is the --dry-run view.)");
    }
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
