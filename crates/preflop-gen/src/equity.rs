//! Preflop equity machinery.
//!
//! Two regimes (design 07): heads-up all-ins read an **exact** 169×169
//! class-vs-class table (enumerated once by `preflop-gen equity`, committed
//! as `data/preflop/equity-hu-169.json`); 3+-way all-ins are Monte-Carlo
//! estimated on first sight and memoized by their sorted class tuple.
//! Everything is class-level: combo-vs-combo blocker effects inside a class
//! average out by construction.

use poker_trainer::preflop::{class_name, CLASSES};
use rand::rngs::SmallRng;
use rand::RngExt;
use rs_poker::core::{Card, Deck, Rankable};
use serde::{Deserialize, Serialize};
use std::io;
use std::path::Path;

/// All concrete combos of a hand class (6 for pairs, 4 suited, 12 offsuit).
pub fn combos_of(class: usize) -> Vec<[Card; 2]> {
    const SUITS: [char; 4] = ['s', 'h', 'd', 'c'];
    let name = class_name(class);
    let mut ch = name.chars();
    let (r1, r2) = (ch.next().unwrap(), ch.next().unwrap());
    let card = |r: char, s: char| Card::try_from(format!("{r}{s}").as_str()).unwrap();
    let mut out = Vec::new();
    match ch.next() {
        None => {
            // pair: C(4,2) suit pairs
            for (i, &s1) in SUITS.iter().enumerate() {
                for &s2 in &SUITS[i + 1..] {
                    out.push([card(r1, s1), card(r2, s2)]);
                }
            }
        }
        Some('s') => {
            for s in SUITS {
                out.push([card(r1, s), card(r2, s)]);
            }
        }
        _ => {
            for &s1 in &SUITS {
                for &s2 in &SUITS {
                    if s1 != s2 {
                        out.push([card(r1, s1), card(r2, s2)]);
                    }
                }
            }
        }
    }
    out
}

/// One canonical combo of a class (any choice works — suit symmetry).
pub fn canonical(class: usize) -> [Card; 2] {
    combos_of(class)[0]
}

/// Exact pot share of `hero_class`'s canonical combo vs every `villain_class`
/// combo over every runout: full C(50,5) board enumeration, ties as half.
/// Fixing one hero combo is exact — suit permutations biject the villain
/// combos — and halves the table work since `e(b,a) = 1 - e(a,b)`.
pub fn exact_pair_equity(hero_class: usize, villain_class: usize) -> f64 {
    let hero = canonical(hero_class);
    let villains: Vec<[Card; 2]> = combos_of(villain_class)
        .into_iter()
        .filter(|v| !v.iter().any(|c| hero.contains(c)))
        .collect();
    let deck: Vec<Card> = Deck::default()
        .into_iter()
        .filter(|c| !hero.contains(c))
        .collect();
    debug_assert_eq!(deck.len(), 50);

    let mut score = 0.0f64;
    let mut count = 0u64;
    let mut hero7 = [hero[0]; 7];
    let mut vill7 = [hero[0]; 7];
    hero7[..2].copy_from_slice(&hero);
    // Boards drawn from the 50 non-hero cards; villain-conflicting boards are
    // skipped per combo, which weights every (villain, board) pair equally.
    for a in 0..46 {
        for b in a + 1..47 {
            for c in b + 1..48 {
                for d in c + 1..49 {
                    for e in d + 1..50 {
                        let board = [deck[a], deck[b], deck[c], deck[d], deck[e]];
                        hero7[2..].copy_from_slice(&board);
                        let hero_rank = hero7[..].rank();
                        vill7[2..].copy_from_slice(&board);
                        for v in &villains {
                            if board.contains(&v[0]) || board.contains(&v[1]) {
                                continue;
                            }
                            vill7[..2].copy_from_slice(v);
                            score += match hero_rank.cmp(&vill7[..].rank()) {
                                std::cmp::Ordering::Greater => 1.0,
                                std::cmp::Ordering::Equal => 0.5,
                                std::cmp::Ordering::Less => 0.0,
                            };
                            count += 1;
                        }
                    }
                }
            }
        }
    }
    score / count as f64
}

/// Enumerate the whole 169×169 table on `threads` OS threads (one unordered
/// pair at a time off a shared counter). Hours of CPU — run via
/// `preflop-gen equity` under `scripts/idle-run.sh`, once.
pub fn gen_hu_table(threads: usize) -> Vec<f64> {
    let pairs: Vec<(usize, usize)> = (0..CLASSES)
        .flat_map(|a| (a..CLASSES).map(move |b| (a, b)))
        .collect();
    let next = std::sync::atomic::AtomicUsize::new(0);
    let done = std::sync::atomic::AtomicUsize::new(0);
    let results: Vec<std::sync::Mutex<Vec<(usize, usize, f64)>>> = (0..threads)
        .map(|_| std::sync::Mutex::new(Vec::new()))
        .collect();

    std::thread::scope(|scope| {
        for slot in &results {
            scope.spawn(|| loop {
                let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let Some(&(a, b)) = pairs.get(i) else { break };
                let eq = exact_pair_equity(a, b);
                slot.lock().unwrap().push((a, b, eq));
                let n = done.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
                if n.is_multiple_of(100) {
                    eprintln!("equity: {n}/{} pairs", pairs.len());
                }
            });
        }
    });

    let mut table = vec![0.0f64; CLASSES * CLASSES];
    for slot in &results {
        for &(a, b, eq) in slot.lock().unwrap().iter() {
            table[a * CLASSES + b] = eq;
            table[b * CLASSES + a] = 1.0 - eq;
        }
    }
    table
}

/// On-disk shape of the HU table (and of the k-way cache values).
#[derive(Serialize, Deserialize)]
struct HuTableFile {
    v: u32,
    /// Row-major 169×169 hero-share table, `e[hero*169+villain]`.
    equity: Vec<f64>,
}

/// Write the table, rounded to 8 decimals (keeps the committed file small;
/// the complement identity then holds to 1e-8, which the tests use).
pub fn save_hu_table(path: impl AsRef<Path>, table: &[f64]) -> io::Result<()> {
    let rounded: Vec<f64> = table.iter().map(|x| (x * 1e8).round() / 1e8).collect();
    let file = HuTableFile {
        v: 1,
        equity: rounded,
    };
    std::fs::write(path, serde_json::to_string(&file)?)
}

/// Load `data/preflop/equity-hu-169.json`.
pub fn load_hu_table(path: impl AsRef<Path>) -> io::Result<Vec<f64>> {
    let path = path.as_ref();
    let text = std::fs::read_to_string(path).map_err(|e| {
        io::Error::new(
            e.kind(),
            format!(
                "{}: {e} — generate it once with `preflop-gen equity` (idle-run)",
                path.display()
            ),
        )
    })?;
    let file: HuTableFile =
        serde_json::from_str(&text).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    if file.equity.len() != CLASSES * CLASSES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "HU table has {} entries, want {}",
                file.equity.len(),
                CLASSES * CLASSES
            ),
        ));
    }
    Ok(file.equity)
}

/// One dealt hand: every seat's cards and classes, plus the undealt rest of
/// the deck (multiway terminals sample runouts from it — blockers exact).
pub struct Deal {
    /// Each seat's 169-class.
    pub classes: [u8; 6],
    /// Each seat's actual hole cards.
    pub holes: [[Card; 2]; 6],
    /// The 52 − 2n cards nobody holds.
    pub pool: Vec<Card>,
}

impl Deal {
    /// A placeholder deal for class-level valuation paths (exact-BR) that
    /// never sample boards: canonical holes, empty pool.
    pub fn class_level(classes: [u8; 6]) -> Deal {
        let mut holes = [[canonical(0)[0]; 2]; 6];
        for (h, &c) in holes.iter_mut().zip(&classes) {
            *h = canonical(c as usize);
        }
        Deal {
            classes,
            holes,
            pool: Vec::new(),
        }
    }
}

/// Pot-share source for showdown terminals: the exact class table heads-up,
/// per-visit sampled runouts multiway.
pub struct EquityCache {
    hu: Vec<f64>,
    /// Sampled runouts per multiway terminal visit. Unbiased and cheap
    /// (~200 × k rank evals); MCCFR's own averaging absorbs the variance.
    // ponytail: a memoized class-tuple cache was tried first and lost: 5/6-way
    // tuples almost never repeat, so it was one fresh 20k-board solve per
    // terminal and unbounded memory. Per-visit sampling from the *actual*
    // dealt cards is bounded and blocker-exact.
    pub sample_boards: u32,
}

impl EquityCache {
    /// Wrap an already-loaded HU table.
    pub fn new(hu: Vec<f64>) -> Self {
        assert_eq!(hu.len(), CLASSES * CLASSES);
        Self {
            hu,
            sample_boards: 200,
        }
    }

    /// Load the committed HU table from `path`.
    pub fn load(path: impl AsRef<Path>) -> io::Result<Self> {
        Ok(Self::new(load_hu_table(path)?))
    }

    /// Exact heads-up pot share of `hero` class vs `villain` class.
    pub fn hu(&self, hero: usize, villain: usize) -> f64 {
        self.hu[hero * CLASSES + villain]
    }
}

/// Monte-Carlo per-player pot shares for concrete `holes`, sampling `boards`
/// runouts from `pool` (the undealt deck). Ties split the pot.
pub fn sampled_shares(
    holes: &[[Card; 2]],
    pool: &[Card],
    boards: u32,
    rng: &mut SmallRng,
) -> Vec<f64> {
    let k = holes.len();
    debug_assert!(pool.len() >= 5, "pool must hold a full runout");
    let mut shares = vec![0.0f64; k];
    let mut board = [holes[0][0]; 5];
    let mut seven = [holes[0][0]; 7];
    let mut ranks = Vec::with_capacity(k);

    for _ in 0..boards {
        // 5 distinct pool indices by rejection (pool ≥ 40 in practice).
        let mut idx = [0usize; 5];
        let mut picked = 0;
        while picked < 5 {
            let i = rng.random_range(0..pool.len());
            if !idx[..picked].contains(&i) {
                idx[picked] = i;
                picked += 1;
            }
        }
        for (b, &i) in board.iter_mut().zip(&idx) {
            *b = pool[i];
        }
        seven[2..].copy_from_slice(&board);
        ranks.clear();
        ranks.extend(holes.iter().map(|h| {
            seven[..2].copy_from_slice(h);
            seven[..].rank()
        }));
        let best = ranks.iter().max().unwrap();
        let winners = ranks.iter().filter(|r| *r == best).count() as f64;
        for (s, r) in shares.iter_mut().zip(&ranks) {
            if r == best {
                *s += 1.0 / winners;
            }
        }
    }
    for s in &mut shares {
        *s /= f64::from(boards);
    }
    shares
}

#[cfg(test)]
mod tests {
    use super::*;
    use poker_trainer::preflop::class_index_of;
    use rand::SeedableRng;

    #[test]
    fn combo_counts_match_class_kinds() {
        assert_eq!(combos_of(class_index_of("AA").unwrap()).len(), 6);
        assert_eq!(combos_of(class_index_of("AKs").unwrap()).len(), 4);
        assert_eq!(combos_of(class_index_of("AKo").unwrap()).len(), 12);
        // Canonical combos are self-consistent (two distinct cards).
        for class in [0, 1, 13, 168] {
            let c = canonical(class);
            assert_ne!(c[0], c[1]);
        }
    }

    #[test]
    fn sampled_shares_land_in_known_bands() {
        let mut rng = SmallRng::seed_from_u64(7);
        let holes = [
            Deal::class_level([0; 6]).holes[0], // AA canonical
            canonical(class_index_of("KK").unwrap()),
            canonical(class_index_of("72o").unwrap()),
        ];
        let pool: Vec<Card> = Deck::default()
            .into_iter()
            .filter(|c| !holes.iter().any(|h| h.contains(c)))
            .collect();
        assert_eq!(pool.len(), 46);
        // 3-way AA vs KK vs 72o: shares sum to 1, AA clearly best, 72o worst.
        let shares = sampled_shares(&holes, &pool, 20_000, &mut rng);
        assert!((shares.iter().sum::<f64>() - 1.0).abs() < 1e-9);
        assert!(shares[0] > 0.55 && shares[0] < 0.75, "AA {shares:?}");
        assert!(shares[2] < 0.15, "72o {shares:?}");
    }

    #[test]
    fn hu_table_files_round_trip_and_validate() {
        let table: Vec<f64> = (0..CLASSES * CLASSES).map(|i| i as f64 / 30000.0).collect();
        let path = std::env::temp_dir().join(format!("pt-hu-{}.json", std::process::id()));
        save_hu_table(&path, &table).unwrap();
        let back = load_hu_table(&path).unwrap();
        assert_eq!(back.len(), CLASSES * CLASSES);
        assert!((back[1000] - table[1000]).abs() < 1e-8);
        std::fs::remove_file(&path).unwrap();

        let err = load_hu_table("no-such-table.json").unwrap_err();
        assert!(err.to_string().contains("preflop-gen equity"), "{err}");
    }

    /// Committed-table sanity + spot re-derivation. Ignored: loads the real
    /// table (absent until `preflop-gen equity` has run) and re-enumerates a
    /// pair from scratch (~10 s release).
    #[test]
    #[ignore = "needs the committed HU table; re-enumerates one pair"]
    fn committed_hu_table_matches_a_fresh_enumeration() {
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../../data/preflop");
        let table = load_hu_table(format!("{dir}/equity-hu-169.json")).unwrap();
        let (aa, kk) = (class_index_of("AA").unwrap(), class_index_of("KK").unwrap());
        // Complement identity across the whole table.
        for a in 0..CLASSES {
            for b in 0..CLASSES {
                let sum = table[a * CLASSES + b] + table[b * CLASSES + a];
                assert!((sum - 1.0).abs() < 2e-8, "{a},{b}: {sum}");
            }
        }
        // The classic: AA vs KK ≈ 0.82.
        let e = table[aa * CLASSES + kk];
        assert!((0.80..0.84).contains(&e), "AA vs KK = {e}");
        // Fresh enumeration agrees to rounding error.
        let fresh = exact_pair_equity(aa, kk);
        assert!((fresh - e).abs() < 1e-8, "fresh {fresh} vs table {e}");
    }
}
