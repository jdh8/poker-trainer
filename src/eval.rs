//! Hand evaluation & equity — backed by `rs_poker`.

use rand::seq::IndexedRandom;
use rs_poker::core::{Card, CoreRank, Deck, Hand, Rankable, Suit};
use std::cmp::Ordering;

/// Hero's equity vs a specific villain hand on a *fixed* board (3–5 cards), by
/// Monte Carlo over random runouts. Returns a win-share in `[0.0, 1.0]`, with
/// ties counted as half. A full 5-card board is exact, so `iters` collapses to
/// one deterministic showdown.
///
/// `rs_poker`'s built-in `MonteCarloGame` can't hold a fixed board, so we run
/// the loop ourselves: deal the remaining runout from the deck, rank both
/// 7-card hands, tally.
pub fn equity(hero: [Card; 2], villain: [Card; 2], board: &[Card], iters: u32) -> f64 {
    debug_assert!((3..=5).contains(&board.len()), "board must be 3–5 cards");
    let known: Vec<Card> = hero.iter().chain(&villain).chain(board).copied().collect();
    let remaining: Vec<Card> = Deck::default()
        .into_iter()
        .filter(|c| !known.contains(c))
        .collect();

    let n_runout = 5 - board.len();
    let iters = if n_runout == 0 { 1 } else { iters }; // 5-card board is exact
    let mut rng = rand::rng();
    let mut score = 0.0; // win = 1.0, tie = 0.5, loss = 0.0
    for _ in 0..iters {
        // distinct runout cards (`sample` = without replacement)
        let tr: Vec<Card> = remaining.sample(&mut rng, n_runout).copied().collect();
        let hero_rank = seven(hero, board, &tr).rank();
        let villain_rank = seven(villain, board, &tr).rank();
        score += match hero_rank.cmp(&villain_rank) {
            Ordering::Greater => 1.0,
            Ordering::Equal => 0.5,
            Ordering::Less => 0.0,
        };
    }
    score / iters as f64
}

/// Mean equity of `hero` vs every combo in `villain_range` that doesn't collide
/// with the hero's cards or the board (3–5 cards). `0.5` if nothing is left to
/// play against.
///
/// ponytail: O(hero × villain) Monte Carlo — fine because the range drill runs
/// this once at startup; keep per-pair `iters` low and the variance averages out
/// across the many villain combos.
pub fn equity_vs_range(
    hero: [Card; 2],
    board: &[Card],
    villain_range: &[[Card; 2]],
    iters: u32,
) -> f64 {
    let blocked = |v: &[Card; 2]| v.iter().any(|c| hero.contains(c) || board.contains(c));
    let live: Vec<&[Card; 2]> = villain_range.iter().filter(|v| !blocked(v)).collect();
    if live.is_empty() {
        return 0.5;
    }
    live.iter()
        .map(|v| equity(hero, **v, board, iters))
        .sum::<f64>()
        / live.len() as f64
}

fn seven(hole: [Card; 2], board: &[Card], runout: &[Card]) -> Hand {
    let mut cards = vec![hole[0], hole[1]];
    cards.extend_from_slice(board);
    cards.extend_from_slice(runout);
    Hand::new_with_cards(cards)
}

/// Coarse made-hand strength of a hole pair on a flop, for the range drill.
/// Declared strong -> weak so it sorts and prints in that order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Bucket {
    Value,    // two pair or better
    Overpair, // a pocket pair above the whole board
    TopPair,  // paired the highest board card
    Pair,     // a weaker pair (second pair, underpair, bottom pair) the hero made
    Draw,     // no made pair, but a flush or straight draw
    Air,      // no pair, no draw
}

impl std::fmt::Display for Bucket {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Each variant's Debug name is exactly its display label.
        write!(f, "{self:?}")
    }
}

/// Bucket the hero's two hole cards on a flop by made-hand strength.
pub fn classify_hand(hole: [Card; 2], flop: [Card; 3]) -> Bucket {
    let cards = [hole[0], hole[1], flop[0], flop[1], flop[2]];
    let category = Hand::new_with_cards(cards.to_vec()).rank().category();
    let has_draw = flush_draw(&cards) || straight_draw(&cards);
    match category {
        CoreRank::TwoPair
        | CoreRank::ThreeOfAKind
        | CoreRank::Straight
        | CoreRank::Flush
        | CoreRank::FullHouse
        | CoreRank::FourOfAKind
        | CoreRank::StraightFlush => Bucket::Value,
        CoreRank::OnePair if hero_makes_pair(hole, flop) => pair_strength(hole, flop),
        // ponytail: a board pair (e.g. 8h8c3d) ranks every hand OnePair, so only count it as a
        // Pair when the hero contributed. Exotic pure-board hands on a trips board aren't gated.
        CoreRank::OnePair | CoreRank::HighCard => {
            if has_draw {
                Bucket::Draw
            } else {
                Bucket::Air
            }
        }
    }
}

/// Split a made single pair into overpair / top pair / weaker, by board rank.
fn pair_strength(hole: [Card; 2], flop: [Card; 3]) -> Bucket {
    let top = flop.iter().map(|c| u8::from(c.value)).max().unwrap();
    let is_pocket = hole[0].value == hole[1].value;
    if is_pocket && u8::from(hole[0].value) > top {
        Bucket::Overpair
    } else if hole.iter().any(|c| u8::from(c.value) == top) {
        Bucket::TopPair
    } else {
        Bucket::Pair
    }
}

/// Did the hero contribute to the pair (pocket pair, or pairing a board card)?
fn hero_makes_pair(hole: [Card; 2], flop: [Card; 3]) -> bool {
    hole[0].value == hole[1].value
        || flop
            .iter()
            .any(|c| c.value == hole[0].value || c.value == hole[1].value)
}

/// Exactly four cards of one suit among the five (a four-flush draw).
fn flush_draw(cards: &[Card; 5]) -> bool {
    let mut counts = [0u8; 4];
    for c in cards {
        let i = match c.suit {
            Suit::Spade => 0,
            Suit::Heart => 1,
            Suit::Diamond => 2,
            Suit::Club => 3,
        };
        counts[i] += 1;
    }
    counts.contains(&4)
}

/// Four distinct ranks inside some five-wide window (open-ender or gutshot),
/// counting the ace as both high (12) and low (-1).
fn straight_draw(cards: &[Card; 5]) -> bool {
    let mut ranks: Vec<i16> = cards.iter().map(|c| u8::from(c.value) as i16).collect();
    if ranks.contains(&12) {
        ranks.push(-1); // ace plays low for the wheel
    }
    ranks.sort_unstable();
    ranks.dedup();
    (-1..=8).any(|lo| {
        ranks
            .iter()
            .filter(|&&r| (lo..=lo + 4).contains(&r))
            .count()
            >= 4
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn card(s: &str) -> Card {
        s.try_into().unwrap()
    }

    #[test]
    fn locked_equity_is_one() {
        // Hero flops a royal flush (the nuts); villain is drawing dead, so hero
        // wins every runout -> equity is exactly 1.0 regardless of turn/river.
        let hero = [card("As"), card("Ks")];
        let villain = [card("2c"), card("2d")];
        let flop = [card("Qs"), card("Js"), card("Ts")];
        assert_eq!(equity(hero, villain, &flop, 1000), 1.0);
    }

    #[test]
    fn nuts_beats_whole_range() {
        // Hero flops the nuts; vs any (live) villain range, equity is 1.0.
        let hero = [card("As"), card("Ks")];
        let flop = [card("Qs"), card("Js"), card("Ts")];
        let range = [
            [card("2c"), card("2d")],
            [card("9h"), card("9c")],
            [card("Ad"), card("Kd")],
            [card("As"), card("2h")], // collides with hero -> skipped
        ];
        assert_eq!(equity_vs_range(hero, &flop, &range, 200), 1.0);
        // Empty / fully-blocked range -> neutral 0.5.
        assert_eq!(equity_vs_range(hero, &flop, &[], 200), 0.5);
    }

    #[test]
    fn turn_and_river_boards_are_exact() {
        // A 5-card board leaves no runout: equity is a single deterministic
        // showdown, so 1 iter and 1000 iters must agree exactly.
        let hero = [card("As"), card("Ks")]; // pair of aces
        let villain = [card("Qh"), card("Jd")]; // pair of queens
        let river = [card("Ah"), card("7c"), card("2d"), card("5s"), card("9h")];
        assert_eq!(equity(hero, villain, &river, 1), 1.0);
        assert_eq!(equity(hero, villain, &river, 1000), 1.0);
        // A 4-card (turn) board still averages a one-card runout.
        let turn = [card("Ah"), card("7c"), card("2d"), card("5s")];
        assert_eq!(equity(hero, villain, &turn, 500), 1.0);
    }

    fn hole(a: &str, b: &str) -> [Card; 2] {
        [card(a), card(b)]
    }
    fn flop(a: &str, b: &str, c: &str) -> [Card; 3] {
        [card(a), card(b), card(c)]
    }

    #[test]
    fn classify_buckets() {
        let board = flop("Td", "9d", "6h");
        // Set on the board -> Value.
        assert_eq!(classify_hand(hole("Ts", "Tc"), board), Bucket::Value);
        // Two pair -> Value.
        assert_eq!(classify_hand(hole("Th", "9s"), board), Bucket::Value);
        // Paired the top board card -> TopPair.
        assert_eq!(classify_hand(hole("Tc", "2s"), board), Bucket::TopPair);
        // Pocket pair above the board -> Overpair.
        assert_eq!(classify_hand(hole("Js", "Jc"), board), Bucket::Overpair);
        // Paired the middle board card -> weaker Pair.
        assert_eq!(classify_hand(hole("9s", "2c"), board), Bucket::Pair);
        // Underpair the hero made -> Pair.
        assert_eq!(classify_hand(hole("4s", "4c"), board), Bucket::Pair);
        // Open-ended straight draw (J8 wants a 7 or Q), no pair -> Draw.
        assert_eq!(classify_hand(hole("Js", "8c"), board), Bucket::Draw);
        // Flush draw, no pair -> Draw.
        assert_eq!(classify_hand(hole("Ad", "2d"), board), Bucket::Draw);
        // Nothing -> Air.
        assert_eq!(classify_hand(hole("Ks", "2c"), board), Bucket::Air);
    }

    #[test]
    fn paired_board_air_is_not_a_pair() {
        // On 8h8c3d the board pair ranks AK as OnePair; the hero contributed
        // nothing, so it must bucket as Air, not Pair.
        let board = flop("8h", "8c", "3d");
        assert_eq!(classify_hand(hole("As", "Kc"), board), Bucket::Air);
        // Pairing a board card on a paired board makes two pair -> Value.
        assert_eq!(classify_hand(hole("Ah", "3c"), board), Bucket::Value);
    }
}
