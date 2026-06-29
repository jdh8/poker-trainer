//! Hand evaluation & equity — backed by `rs_poker`.

use rand::seq::IndexedRandom;
use rs_poker::core::{Card, Deck, Hand, Rankable};
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
}
