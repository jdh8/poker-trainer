//! Hand evaluation & equity — backed by `rs_poker`.

use rand::seq::IndexedRandom;
use rs_poker::core::{Card, CoreRank, Deck, Hand, Rankable, Suit};
use std::cmp::Ordering;

/// Hero's equity vs a specific villain hand on a *fixed* flop, by Monte Carlo
/// over random turn+river runouts. Returns a win-share in `[0.0, 1.0]`, with
/// ties counted as half.
///
/// `rs_poker`'s built-in `MonteCarloGame` can't hold a fixed board, so we run
/// the loop ourselves: deal turn+river from the remaining deck, rank both
/// 7-card hands, tally.
pub fn equity(hero: [Card; 2], villain: [Card; 2], flop: [Card; 3], iters: u32) -> f64 {
    let known = [
        hero[0], hero[1], villain[0], villain[1], flop[0], flop[1], flop[2],
    ];
    let remaining: Vec<Card> = Deck::default()
        .into_iter()
        .filter(|c| !known.contains(c))
        .collect();

    let mut rng = rand::rng();
    let mut score = 0.0; // win = 1.0, tie = 0.5, loss = 0.0
    for _ in 0..iters {
        // two distinct cards for turn+river (`sample` = without replacement)
        let tr: Vec<Card> = remaining.sample(&mut rng, 2).copied().collect();
        let hero_rank = seven(hero, flop, &tr).rank();
        let villain_rank = seven(villain, flop, &tr).rank();
        score += match hero_rank.cmp(&villain_rank) {
            Ordering::Greater => 1.0,
            Ordering::Equal => 0.5,
            Ordering::Less => 0.0,
        };
    }
    score / iters as f64
}

fn seven(hole: [Card; 2], flop: [Card; 3], turn_river: &[Card]) -> Hand {
    Hand::new_with_cards(vec![
        hole[0],
        hole[1],
        flop[0],
        flop[1],
        flop[2],
        turn_river[0],
        turn_river[1],
    ])
}

/// Coarse made-hand strength of a hole pair on a flop, for the range drill.
/// Declared strong -> weak so it sorts and prints in that order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Bucket {
    Value, // two pair or better
    Pair,  // a single pair the hero actually helped make
    Draw,  // no made pair, but a flush or straight draw
    Air,   // no pair, no draw
}

impl std::fmt::Display for Bucket {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Bucket::Value => "Value",
            Bucket::Pair => "Pair",
            Bucket::Draw => "Draw",
            Bucket::Air => "Air",
        };
        f.write_str(s)
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
        CoreRank::OnePair if hero_makes_pair(hole, flop) => Bucket::Pair,
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
        assert_eq!(equity(hero, villain, flop, 1000), 1.0);
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
        // Top pair -> Pair.
        assert_eq!(classify_hand(hole("Tc", "2s"), board), Bucket::Pair);
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
