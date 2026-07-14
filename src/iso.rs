//! Suit isomorphism for flops (design doc 08): canonicalize a flop to its
//! representative among the standard 1,755 classes and translate solved nodes
//! between suit spaces, so one stored table serves every suit-isomorphic flop.
//!
//! Exactness: every range in play is class-level ("AA:0.62,AKs:…"), hence
//! suit-symmetric, so a canonical-flop equilibrium transfers to any real flop
//! by relabeling suits — no approximation. Pure card math; this crate never
//! links the solver. The card-id encoding (`rank*4 + suit`, suits in `cdhs`
//! order) and the canonical form (smallest sorted id triple over the 24 suit
//! relabelings) mirror solve-gen's `iso_flops()` and are pinned against it by
//! a solve-gen test.
//!
//! Orientation: [`canonical_flop`] returns the map *this flop → canonical*.
//! [`crate::tree::TableWalk`] holds the composed map *user → stored* and
//! translates inbound cards with [`SuitPerm::card`], outbound nodes with
//! [`translate_node`] (which applies the inverse).

use crate::tree::TreeNode;

const RANKS: &[u8; 13] = b"23456789tjqka";
const SUITS: &[u8; 4] = b"cdhs";

/// Card id in the solver's encoding: `rank*4 + suit`, `2c = 0 … As = 51`.
/// Case-insensitive (table filenames are lowercased wholesale, so be no
/// stricter); `None` for anything that isn't a 2-char card.
fn card_id(card: &str) -> Option<u8> {
    let mut it = card.chars();
    let (r, s) = (it.next()?, it.next()?);
    if it.next().is_some() {
        return None;
    }
    let r = RANKS
        .iter()
        .position(|&b| b == r.to_ascii_lowercase() as u8)?;
    let s = SUITS
        .iter()
        .position(|&b| b == s.to_ascii_lowercase() as u8)?;
    Some((r * 4 + s) as u8)
}

/// The solver's display form: uppercase rank, lowercase suit (`"Td"`).
fn card_str(id: u8) -> String {
    let r = RANKS[(id >> 2) as usize].to_ascii_uppercase() as char;
    let s = SUITS[(id & 3) as usize] as char;
    format!("{r}{s}")
}

/// `"Td9d6h"` / `"td 9d 6h"` → 3 distinct card ids, else `None`.
fn parse_flop(flop: &str) -> Option<[u8; 3]> {
    let s: String = flop.chars().filter(|c| !c.is_whitespace()).collect();
    if !s.is_ascii() || s.len() != 6 {
        return None;
    }
    let ids = [card_id(&s[0..2])?, card_id(&s[2..4])?, card_id(&s[4..6])?];
    if ids[0] == ids[1] || ids[0] == ids[2] || ids[1] == ids[2] {
        return None;
    }
    Some(ids)
}

/// A suit relabeling: `map[s]` is the image of suit `s` (0=c 1=d 2=h 3=s).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SuitPerm([u8; 4]);

impl SuitPerm {
    /// The do-nothing relabeling.
    pub fn identity() -> Self {
        SuitPerm([0, 1, 2, 3])
    }

    /// Whether this perm relabels nothing (translation can be skipped).
    pub fn is_identity(&self) -> bool {
        self.0 == [0, 1, 2, 3]
    }

    /// The relabeling that undoes this one.
    pub fn inverse(&self) -> Self {
        let mut inv = [0u8; 4];
        for (s, &t) in self.0.iter().enumerate() {
            inv[t as usize] = s as u8;
        }
        SuitPerm(inv)
    }

    /// `self ∘ other`: apply `other` first, then `self`.
    pub fn compose(&self, other: &Self) -> Self {
        SuitPerm([0usize, 1, 2, 3].map(|s| self.0[other.0[s] as usize]))
    }

    fn id(&self, id: u8) -> u8 {
        (id & !3) | self.0[(id & 3) as usize]
    }

    /// Map one card string through the perm. `None` if it doesn't parse.
    pub fn card(&self, card: &str) -> Option<String> {
        Some(card_str(self.id(card_id(card)?)))
    }

    /// Map a two-card hand (`"AsKs"`) and re-render it the solver's way:
    /// higher card id first.
    pub fn hand(&self, hand: &str) -> Option<String> {
        if !hand.is_ascii() || hand.len() != 4 {
            return None;
        }
        let a = self.id(card_id(&hand[..2])?);
        let b = self.id(card_id(&hand[2..])?);
        let (hi, lo) = if a >= b { (a, b) } else { (b, a) };
        Some(format!("{}{}", card_str(hi), card_str(lo)))
    }
}

/// All 24 suit relabelings, identity first — the tie-break order everywhere
/// (same nested-loop enumeration as solve-gen's `iso_flops`).
fn perms() -> Vec<SuitPerm> {
    let mut out = Vec::with_capacity(24);
    for a in 0..4u8 {
        for b in 0..4u8 {
            for c in 0..4u8 {
                for d in 0..4u8 {
                    if a != b && a != c && a != d && b != c && b != d && c != d {
                        out.push(SuitPerm([a, b, c, d]));
                    }
                }
            }
        }
    }
    out
}

/// The canonical representative of `flop` among the 1,755 suit-isomorphism
/// classes, plus the relabeling that takes this flop's cards onto it. Ties
/// (paired boards, the unused 4th suit) resolve to the first minimizing perm
/// in enumeration order, so the result is deterministic — and any minimizer
/// is game-exact because ranges are suit-symmetric. `None` unless `flop` is
/// exactly 3 distinct parseable cards.
pub fn canonical_flop(flop: &str) -> Option<(String, SuitPerm)> {
    let ids = parse_flop(flop)?;
    let mut best: Option<([u8; 3], SuitPerm)> = None;
    for p in perms() {
        let mut f = ids.map(|c| p.id(c));
        f.sort_unstable();
        if best.is_none_or(|(b, _)| f < b) {
            best = Some((f, p));
        }
    }
    let (f, p) = best?;
    Some((f.map(card_str).join(""), p))
}

/// `idx`-ordered copy of `v`.
fn permuted<T: Clone>(v: &[T], idx: &[usize]) -> Vec<T> {
    idx.iter().map(|&i| v[i].clone()).collect()
}

/// Translate a stored node into the user's suit space. `to_stored` is the
/// user→stored map; every card field maps through its inverse, then gets
/// re-ordered exactly as a live solve of the user's flop would serve it:
/// board (flop cards) and `dealable` ascending by card id, hands ascending by
/// `(low, high)` id and re-rendered high-card-first, with `freqs`/`evs`
/// columns and `weights`/`equity` permuted in lockstep. Those orderings are
/// load-bearing, not cosmetic — the lock editor ships `[action][hand]`
/// strategies index-parallel to a live game's hand order, and the hand drill
/// looks hands up by string equality across a live fallback.
pub fn translate_node(mut node: TreeNode, to_stored: &SuitPerm) -> TreeNode {
    let back = to_stored.inverse();

    for c in &mut node.board {
        if let Some(mapped) = back.card(c) {
            *c = mapped;
        }
    }
    let flop = node.board.len().min(3);
    node.board[..flop].sort_by_key(|c| card_id(c).unwrap_or(u8::MAX));

    for c in &mut node.dealable {
        if let Some(mapped) = back.card(c) {
            *c = mapped;
        }
    }
    node.dealable.sort_by_key(|c| card_id(c).unwrap_or(u8::MAX));

    for label in &mut node.line {
        if let Some(mapped) = label.strip_prefix("deal ").and_then(|card| back.card(card)) {
            *label = format!("deal {mapped}");
        }
    }

    if !node.hands.is_empty() {
        for h in &mut node.hands {
            if let Some(mapped) = back.hand(h) {
                *h = mapped;
            }
        }
        let key = |h: &str| {
            if h.is_ascii() && h.len() == 4 {
                if let (Some(a), Some(b)) = (card_id(&h[..2]), card_id(&h[2..])) {
                    return (a.min(b), a.max(b));
                }
            }
            (u8::MAX, u8::MAX)
        };
        let mut idx: Vec<usize> = (0..node.hands.len()).collect();
        idx.sort_by_key(|&i| key(&node.hands[i]));
        node.hands = permuted(&node.hands, &idx);
        if node.weights.len() == idx.len() {
            node.weights = permuted(&node.weights, &idx);
        }
        if node.equity.len() == idx.len() {
            node.equity = permuted(&node.equity, &idx);
        }
        for row in &mut node.freqs {
            if row.len() == idx.len() {
                *row = permuted(row, &idx);
            }
        }
        for row in &mut node.evs {
            if row.len() == idx.len() {
                *row = permuted(row, &idx);
            }
        }
    }
    node
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codec_round_trips_and_rejects_garbage() {
        assert_eq!(card_id("2c"), Some(0));
        assert_eq!(card_id("As"), Some(51));
        assert_eq!(card_id("Td"), Some(33));
        assert_eq!(card_id("td"), Some(33));
        assert_eq!(card_id("TD"), Some(33));
        assert_eq!(card_str(33), "Td");
        assert_eq!(card_str(0), "2c");
        assert_eq!(card_str(51), "As");
        for bad in ["T", "Txd", "1c", "Tx", "", "Tdd"] {
            assert_eq!(card_id(bad), None, "{bad:?}");
        }
        assert_eq!(parse_flop("Td 9d 6h"), parse_flop("td9d6h"));
        assert_eq!(parse_flop("TdTd6h"), None, "duplicate card");
        assert_eq!(parse_flop("Td9d"), None);
        assert_eq!(parse_flop("Td9d6h2c"), None);
    }

    #[test]
    fn perm_inverse_compose_and_hand_rendering() {
        let swap_cd_hs = SuitPerm([1, 0, 3, 2]);
        assert_eq!(swap_cd_hs.inverse(), swap_cd_hs);
        assert!(swap_cd_hs.compose(&swap_cd_hs.inverse()).is_identity());
        assert_eq!(swap_cd_hs.card("Td").as_deref(), Some("Tc"));
        assert_eq!(swap_cd_hs.hand("AsKs").as_deref(), Some("AhKh"));
        // Re-rendered high-card-first even when the suits don't move.
        assert_eq!(SuitPerm::identity().hand("QcQd").as_deref(), Some("QdQc"));
        assert_eq!(swap_cd_hs.card("garbage"), None);
    }

    #[test]
    fn canonical_flop_pins_known_values() {
        // Monotone trips relabel onto the smallest suits in order.
        let (canon, p) = canonical_flop("2d2h2s").unwrap();
        assert_eq!(canon, "2c2d2h");
        assert_eq!(p.card("2d").as_deref(), Some("2c"));
        // A canonical flop is its own fixed point under the identity.
        let (canon, p) = canonical_flop("2c2d2h").unwrap();
        assert_eq!(canon, "2c2d2h");
        assert!(p.is_identity());
        // Two-tone: 6h maps to the free smallest suit, the d pair stays.
        let (canon, _) = canonical_flop("Td9d6h").unwrap();
        assert_eq!(canon, "6c9dTd");
        // Order/case-insensitive on input.
        assert_eq!(canonical_flop("9dTD6h").unwrap().0, "6c9dTd");
        assert_eq!(canonical_flop("bogus"), None);
    }

    #[test]
    fn canonical_flop_is_deterministic_on_stabilizer_boards() {
        // Paired board: two relabelings reach the canonical form; the pick
        // must be stable and must actually map the input onto the canonical.
        let (canon, p) = canonical_flop("8h8c3d").unwrap();
        let (canon2, p2) = canonical_flop("8h8c3d").unwrap();
        assert_eq!(canon, canon2);
        assert_eq!(p, p2);
        let mut mapped: Vec<String> = ["8h", "8c", "3d"]
            .iter()
            .map(|c| p.card(c).unwrap())
            .collect();
        mapped.sort_by_key(|c| card_id(c).unwrap());
        assert_eq!(mapped.join(""), canon);
    }

    #[test]
    fn translate_node_reorders_everything_in_lockstep() {
        // Stored space Td9d6h, user space Ts9s6h: user→stored swaps d↔s.
        let to_stored = SuitPerm([0, 3, 2, 1]);
        let stored = TreeNode {
            player: "oop".into(),
            board: vec!["6h".into(), "9d".into(), "Td".into()],
            pot_bb: 6.0,
            line: vec!["Check".into(), "deal 2d".into()],
            actions: vec!["Check".into(), "Bet 2.0bb".into()],
            dealable: vec!["2c".into(), "2d".into()],
            // Keys (0,1) then (0,3) in stored space — the swap must re-sort.
            hands: vec!["2d2c".into(), "2s2c".into()],
            freqs: vec![vec![0.6, 0.4], vec![0.4, 0.6]],
            evs: vec![vec![1.0, 2.0], vec![3.0, 4.0]],
            weights: vec![0.1, 0.2],
            equity: vec![0.5, 0.7],
        };

        let user = translate_node(stored, &to_stored);
        assert_eq!(user.board, vec!["6h", "9s", "Ts"]);
        assert_eq!(user.line, vec!["Check", "deal 2s"]);
        assert_eq!(user.actions, vec!["Check", "Bet 2.0bb"], "labels untouched");
        assert_eq!(user.dealable, vec!["2c", "2s"], "translated and sorted");
        // Stored "2d2c"→user (2s,2c) key (0,3); stored "2s2c"→user (2d,2c)
        // key (0,1): the user-space order flips, and every parallel array
        // must flip with it.
        assert_eq!(user.hands, vec!["2d2c", "2s2c"]);
        assert_eq!(user.weights, vec![0.2, 0.1]);
        assert_eq!(user.equity, vec![0.7, 0.5]);
        assert_eq!(user.freqs, vec![vec![0.4, 0.6], vec![0.6, 0.4]]);
        assert_eq!(user.evs, vec![vec![2.0, 1.0], vec![4.0, 3.0]]);
    }

    #[test]
    fn translate_node_with_identity_only_normalizes_order() {
        let node = TreeNode {
            board: vec!["Td".into(), "9d".into(), "6h".into()],
            hands: vec!["AsKs".into()],
            weights: vec![1.0],
            ..Default::default()
        };
        let out = translate_node(node, &SuitPerm::identity());
        assert_eq!(out.board, vec!["6h", "9d", "Td"]);
        assert_eq!(out.hands, vec!["AsKs"]);
    }
}
