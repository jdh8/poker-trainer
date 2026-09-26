//! Exact flop showdown equity matrix: for one flop, every ordered pair of
//! hole-card combos' equity over all 990 turn+river runouts (ties count half;
//! `0` where the combos share a card or hit the board). Written as a raw
//! little-endian `f32[1326][1326]` in the table combo order (`rank*4 + suit`
//! card ids, combo `(hi, lo)` at `hi*(hi-1)/2 + lo`) — the all-in leaf of the
//! design-09 phase (c) depth-limited solve (`train/dls.py`).
//!
//! ponytail: brute-force pair loop (~600M compares, a few seconds per flop);
//! sort-by-rank counting is the upgrade if it ever runs over many flops.

use clap::Parser;
use poker_trainer::iso;
use rs_poker::core::{Card, Deck, Hand, Rankable, Suit};
use std::cmp::Ordering;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::PathBuf;

const N: usize = 52 * 51 / 2;

#[derive(Parser)]
#[command(about = "Exact 1326×1326 flop showdown equity matrix (raw f32)")]
struct Args {
    /// Flop as packed cards, e.g. `2c7d7h`.
    flop: String,
    /// Output file (raw little-endian `f32[1326][1326]`).
    #[arg(long)]
    out: PathBuf,
}

fn card_id_of(c: Card) -> usize {
    let suit = match c.suit {
        Suit::Club => 0,
        Suit::Diamond => 1,
        Suit::Heart => 2,
        Suit::Spade => 3,
    };
    (c.value as usize) * 4 + suit
}

fn main() -> std::io::Result<()> {
    let args = Args::parse();
    let mut by_id: Vec<Option<Card>> = vec![None; 52];
    for c in Deck::default() {
        by_id[card_id_of(c)] = Some(c);
    }
    let cards: Vec<Card> = by_id.into_iter().map(|c| c.unwrap()).collect();
    let flop: Vec<usize> = (0..3)
        .map(|i| {
            args.flop
                .get(2 * i..2 * i + 2)
                .and_then(iso::card_id)
                .map(usize::from)
                .unwrap_or_else(|| panic!("bad flop {:?}", args.flop))
        })
        .collect();
    let combos: Vec<(usize, usize)> = (0..52)
        .flat_map(|hi| (0..hi).map(move |lo| (hi, lo)))
        .collect();
    let (mut win, mut tie, mut cnt) = (vec![0u16; N * N], vec![0u16; N * N], vec![0u16; N * N]);
    let live: Vec<usize> = (0..52).filter(|c| !flop.contains(c)).collect();

    for (ai, &t) in live.iter().enumerate() {
        for &r in &live[ai + 1..] {
            let dead = |c: usize| c == t || c == r || flop.contains(&c);
            let ranked: Vec<(usize, _)> = combos
                .iter()
                .enumerate()
                .filter(|(_, &(hi, lo))| !dead(hi) && !dead(lo))
                .map(|(i, &(hi, lo))| {
                    let hand = Hand::new_with_cards(vec![
                        cards[hi],
                        cards[lo],
                        cards[flop[0]],
                        cards[flop[1]],
                        cards[flop[2]],
                        cards[t],
                        cards[r],
                    ]);
                    (i, hand.rank())
                })
                .collect();
            for (a, &(i, ri)) in ranked.iter().enumerate() {
                let (hi_i, lo_i) = combos[i];
                for &(j, rj) in &ranked[a + 1..] {
                    let (hi_j, lo_j) = combos[j];
                    if hi_i == hi_j || hi_i == lo_j || lo_i == hi_j || lo_i == lo_j {
                        continue;
                    }
                    cnt[i * N + j] += 1;
                    cnt[j * N + i] += 1;
                    match ri.cmp(&rj) {
                        Ordering::Greater => win[i * N + j] += 1,
                        Ordering::Less => win[j * N + i] += 1,
                        Ordering::Equal => {
                            tie[i * N + j] += 1;
                            tie[j * N + i] += 1;
                        }
                    }
                }
            }
        }
    }

    let mut out = BufWriter::new(File::create(&args.out)?);
    for k in 0..N * N {
        let e = if cnt[k] == 0 {
            0.0
        } else {
            (f32::from(win[k]) + 0.5 * f32::from(tie[k])) / f32::from(cnt[k])
        };
        out.write_all(&e.to_le_bytes())?;
    }
    out.flush()
}
