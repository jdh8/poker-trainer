# Rake & ante: real-world calibration survey (2024–2026)

Background note for [design 07](design/07-preflop-solver.md). The preflop generator
(`crates/preflop-gen`) exposes two economy knobs whose "correct" values were never
pinned to evidence: per-pot **rake** (`rake_rate`/`rake_cap_bb`) and per-player **ante**
(`ante_bb`). The shipped cash ladder uses `rake_rate = 0.05`, `rake_cap_bb = 3.0`,
`ante_bb = 0.0`. This surveys how common rake and ante actually are across popular online
platforms and well-known tournaments, checks those choices against the evidence, and
records the one gap it surfaces. **Survey only — no manifest or solver change followed
from it.**

## Part 1 — Rake: essentially universal; the cash-ladder values are textbook

Rake exists in virtually every real-money game; only home games and rare promos escape
it. What varies is magnitude and form, not existence — Wikipedia: "generally 2.5% to 10%
of the pot… up to a predetermined maximum."

**Online cash — ~5% of pot, small cap, "no flop no drop" is the near-universal
structure:**

| Site | Rate | NL100 cap | Note |
|---|---|---|---|
| PokerStars | 5% (4.5% at NL25 & NL1000) | ~$2.50 | no-flop-no-drop |
| GGPoker | 5% | ~$5 | **Rush & Cash = 5% capped at 3 big blinds**; also rakes 3-bet-preflop pots |
| WPT Global | **4%** | — | lowest headline % on the market |
| ACR / WPN | 5% | **flat $3** at every stake | cheapest deep |

The dollar cap is roughly constant while blinds grow, so the cap **measured in big blinds
shrinks as stakes rise**; rakeback (GG 24–80%, ACR ~27%) cuts the net cost further.

**Online MTT** — expressed as buy-in + fee ($100 + $9 ≈ 9% is the canonical mid-stakes
figure), tapering to ~2–5% at high buy-ins.

**Live MTT** — 2024 WSOP: **8.77% overall**, Main Event **7.0%**, scaling from ~15–18% on
the cheapest events down to ~0.4–3% on super high rollers.

**Live cash** — pot drop ~$1 per $10–20, capped ~$4–6, or a time charge ~$7–15 per half
hour; usually "no flop, no drop."

**Strategic effect — rake tightens ranges (the mirror image of antes).** Rake skims what
you *keep* from pots you win, so marginal opens, flats and limps that are ~break-even at
zero rake go negative; solvers solved with rake return **tighter** ranges, and the effect
bites hardest at low stakes (the cap is a big fraction of the pot), on small/multiway
pots, and on calling/defending. Two structural escapes: fold-wins are unraked ("no flop
no drop"), so pure blind steals are unaffected, and once the cap binds the marginal chips
are rake-free, so deep/big-pot decisions barely move. **Antes broaden, rake tightens —
same lever (the size of the contested pot), opposite signs.**

**Verdict for the trainer:** the cash ladder's `rake_rate = 0.05`, `rake_cap_bb = 3.0`,
fold-wins unraked is a **textbook match to the online-cash norm — literally GGPoker
Rush & Cash's structure (5% capped at 3 bb).** Validated; no change warranted. (Only nit:
GG also rakes 3-bet-preflop pots, a minor deviation from pure "no flop no drop" not worth
modeling.)

## Part 2 — Ante: universal in tournaments (BB ante), absent in cash

### 2a. Live majors: Big Blind Ante = 1 BB is the standard

| Series | Method | Ante | Since |
|---|---|---|---|
| WSOP (live) | Big blind ante | = 1 BB, from L1 | all NLHE since 2019 |
| EPT / PokerStars Live | Big blind ante | = 1 BB, from L1 | 2018 |
| WPT (live) | Big blind ante | = 1 BB | 2018 |
| partypoker MILLIONS | Big blind ante | = 1 BB | Nov 2018 (from button ante) |
| Triton SHR (NLH) | Big blind ante | = 1 BB, from L1 | current structures |
| Aussie Millions | Big blind ante | = 1 BB | current |

The BBA is posted by the big blind and equals one big blind at **every** level (TDA RP-11:
don't reduce it). Niche exception: Triton **Short Deck** is button/double-ante, ante-only
(no blinds).

### 2b. Online: traditional per-player ante, same total effect

PokerStars and GGPoker online MTTs use a **traditional per-player ante ~10–15% of the BB,
from Level 1** (PokerStars Sunday Million L1 = 50/100 ante 12). Online software
auto-collects, so the *speed* rationale that pushed live rooms to the BBA doesn't apply —
but the **total dead money is the same ~1 BB across the table**, so the equilibrium effect
is identical. Traditional live antes were historically ~1/8 BB (≈12.5%) per player, table
total ≈ 1 BB — exactly what the single-1-BB BBA was designed to replicate.

**Net:** with either mechanism, preflop dead money ≈ **1.5 BB** (1 BB ante + 0.5 SB)
before anyone acts — i.e. the ante makes the preflop pot **~40% bigger** than a no-ante
game.

### 2c. Cash: blinds-only, no ante

Standard NLHE cash (especially online) is blinds-only. Button-ante/straddle appear only in
niche high-stakes streamed games (Hustler Casino Live runs $100/$200 with a $200 BB ante).
Not worth modeling for a cash trainer. → the ladder's `ante_bb = 0.0` is **correct for
cash**.

### 2d. Format coverage — antes are in essentially every serious MTT format

Freezeout, rebuy/re-entry, **PKO / bounty**, satellites, and turbos all carry antes via
the BBA. PKO is a **dominant modern online format** (GGPoker Bounty Hunters runs $50–100M
guarantees) and layers a *second* dead-money source on top of the ante. The one real
exception is **Spin & Go / lottery hyper-SNGs — blinds-only, no ante** (which is why they
collapse to pure blind-vs-blind push/fold late).

### 2e. Where ante-relevant preflop EV actually lives (stack-depth arc)

MTTs start deep (100–200+ BB) but the **decision-weighted** time concentrates far shorter,
with the ante always present:

- pure push/fold (jam-or-fold): **≤ 10–15 BB**
- reshove / jam-over-open: **~15–35 BB**
- mid/late playable band: **~26–40 BB**
- money bubble / final table: **~10–30 BB** (ICM pushes it shorter still)
- heads-up: often **< 20 BB**

M with a BB ante ≈ BB depth ÷ 2.5 (the orbit costs SB + BB + 1-BB ante = 2.5 BB). **A cash
game lives at ~100 BB with no ante; a tournament lives at ~10–40 BB with an ante. The two
regimes barely overlap.**

### 2f. How much antes move preflop strategy (why it's a distinct training target)

- opening/steal frequencies rise **~4–6%** (e.g. SB ~68% at 20 BB to a 2×)
- BB defense vs a 9-BB shove: required equity drops **~44% → ~40%**
- short-stack jam ranges widen **~10–15%+** on both sides
- solvers ship **separate ante vs no-ante range packs** — a no-ante chart is simply the
  wrong equilibrium once antes are live
- PKO drops the call threshold further, ~**43.6% → 32.4%** in GTO Wizard's example

## Part 3 — Conclusions for the trainer

1. **Rake is validated as-is.** 5% capped at 3 bb, fold-wins unraked = the online-cash
   norm. No action.
2. **`ante_bb = 0` is correct for the cash ladder.** Cash is blinds-only.
3. **The gap: the trainer ships zero tournament rulesets, yet antes are universal in
   tournaments.** The engine already supports `ante_bb` + ICM (design 07 §Solver core), so
   the capability is idle, not missing. Because the tournament regime (~10–40 BB,
   ante-present) barely overlaps the cash regime (~100 BB, no ante), an ante ladder would
   train a **genuinely different, wider** range set — not a variation on the cash charts.

**Implementation notes for whenever a tournament ladder is actually built (not now):**

- Tournament "rake" is an **entry fee**, not a per-pot drop → a tournament ruleset sets
  `rake_rate = 0` (the juice is sunk at buy-in and doesn't enter per-hand EV).
- The engine's `ante_bb` is a **per-player dead ante** (matches the traditional/online
  model). The BBA's total-1-BB effect is reproduced by `ante_bb ≈ 1/seats`
  (6-max → 0.167): dead antes are strategically equivalent by *total* amount for chip-EV,
  so **no `game.rs` change is needed** for a first cut. A true "only the BB posts 1 BB"
  mechanic matters only under ICM (whose stack the ante leaves).
- Depths should cluster in **~10–40 BB, jam-heavy**, not the cash Fibonacci ladder's deep
  rungs. ICM (already implemented) is where bubble/final-table realism lives; a chip-EV
  ante ladder is the cheaper first cut.

## Sources

Rake: [Wikipedia: Rake](https://en.wikipedia.org/wiki/Rake_(poker)) ·
[PokerStars rake](https://www.pokerstars.com/poker/room/rake/) ·
[GGPoker rake/cap](https://help.ggpoker.com/article/914-understanding-blind-structure-rake-percentage-and-cap) ·
[WPT Global rake](https://wptglobal.com/poker-table-rake) ·
[2024 WSOP rake (PokerNews)](https://www.pokernews.com/news/2024/07/2024-world-series-of-poker-rake-and-statistics-46652.htm).

Ante: [Big blind ante (PokerNews)](https://www.pokernews.com/pokerterms/big-blind-ante.htm) ·
[BB ante becomes standard, 2018](https://www.pokernews.com/news/2018/12/top-10-stories-of-2018-10-big-blind-ante-32831.htm) ·
[2025 WSOP ME structure](https://wsop.gg-global-cdn.com/wsop/pdfs/structuresheets/structure_5771_24782.pdf) ·
[Triton Paradise 2024 structure](https://cdn.triton-series.com/wp-content/uploads/2019/12/26142153/Triton-Poker-X-WSOP-Paradise_Structure_241019.pdf) ·
[Sunday Million structure (online per-player ante)](https://www.pokernews.com/tours/other-tournaments/sunday-million-14th-anniversary-edition/sunday-million-14th-anniversary-edition/structure.htm) ·
[Antes strategy (Upswing)](https://upswingpoker.com/poker-antes-tournament-raising-strategy/) ·
[PKO theory (GTO Wizard)](https://blog.gtowizard.com/the-theory-of-progressive-knockout-tournaments/) ·
[Common stack sizes (PokerNews)](https://www.pokernews.com/strategy/keep-your-tournament-game-simple-stack-sizes-25067.htm) ·
[M-ratio](https://en.wikipedia.org/wiki/M-ratio).
