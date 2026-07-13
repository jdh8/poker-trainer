// Framework-free glue: wasm exports return JSON strings, we parse and render.
// The GTO grid and preflop chart sections never touch wasm — they fetch the
// committed JSON directly.
import init, { equity_report, equity_vs_reach, made_hand } from './pkg/poker_trainer_web.js';

const $ = id => document.getElementById(id);
const SUITS = { s: ['♠', 'spade'], h: ['♥', 'heart'], d: ['♦', 'diamond'], c: ['♣', 'club'] };
const cardHTML = code => {
  const [sym, cls] = SUITS[code[1]];
  return `<span class="card ${cls}">${code[0]}${sym}</span>`;
};
const cardsHTML = codes => codes.map(cardHTML).join('');
const pct = x => (x * 100).toFixed(1) + '%';

// ---- equity calculator ------------------------------------------------------

function histogramHTML(label, bins, n, mean) {
  const peak = Math.max(...bins, 1);
  const rows = bins.map((c, i) =>
    `<div class="row"><span class="lbl">${i * 10}–${i * 10 + 10}%</span>` +
    `<span class="bar" style="width:${(c / peak) * 60}%"></span><span class="n">${c}</span></div>`
  ).join('');
  const head = mean === null ? `${label} (n=${n})`
    : `${label} equity distribution (n=${n} combos, mean ${pct(mean)})`;
  return `<div class="hist"><div>${head}</div>${rows}</div>`;
}

$('eq-run').onclick = () => {
  const out = $('eq-out');
  out.textContent = 'Computing…';
  setTimeout(() => {   // let the label paint before the synchronous MC loop
    try {
      const r = JSON.parse(equity_report($('eq-oop').value, $('eq-ip').value, $('eq-board').value.trim()));
      out.innerHTML =
        `<p class="spotline">Board ${cardsHTML(r.board.match(/.{2}/g))} — ` +
        `OOP <b>${pct(r.oop_mean)}</b> vs IP <b>${pct(1 - r.oop_mean)}</b></p>` +
        histogramHTML('OOP', r.oop_bins, r.oop_n, r.oop_mean) +
        histogramHTML('IP', r.ip_bins, r.ip_n, null);
    } catch (e) {
      out.textContent = 'Error: ' + (e.message || e);
    }
  }, 20);
};

// ---- pot-odds drill ---------------------------------------------------------

let poSpot = null, poRight = 0, poTotal = 0;
// The selected HU ruleset's path->node map, its header, and effective stack for
// the all-in guard (loaded on init and on every source change).
let poSource = '', poNodes = null, poHeader = null, poStack = null;

$('po-deal').onclick = () => {
  if (!poNodes) return;   // charts still loading
  const spot = preflopPotOddsSpot();
  if (!spot) {   // 200 sims never reached a non-all-in flop
    $('po-spot').innerHTML = '<p>This ruleset rarely reaches a non-all-in flop — ' +
      'try a deeper HU set like cash-hu89.</p>';
    $('po-actions').hidden = true;
    $('po-reveal').innerHTML = '';
    return;
  }
  poSpot = spot;
  const line = spot.line && spot.line.length
    ? `<p class="spotline">Preflop: ${spot.line.join(', ')}.</p>` : '';
  $('po-spot').innerHTML = line +
    `<p class="spotline">Your hand: ${cardsHTML(spot.hero)} &nbsp; Flop: ${cardsHTML(spot.flop)}</p>` +
    `<p>Pot ${spot.pot.toFixed(1)}bb. Villain bets <b>${spot.bet.toFixed(1)}bb</b> — ` +
    `call ${spot.bet.toFixed(1)} to win ${(spot.pot + spot.bet).toFixed(1)}, ` +
    `so you need <b>${pct(spot.required)}</b> equity.</p>`;
  $('po-actions').hidden = false;
  $('po-reveal').innerHTML = '';
};

function poAnswer(called) {
  if (!poSpot) return;
  const s = poSpot; poSpot = null;
  $('po-actions').hidden = true;
  const right = called === s.should_call;
  poTotal++; if (right) poRight++;
  $('po-score').textContent = `${poRight}/${poTotal} correct`;
  $('po-reveal').innerHTML =
    `<p>Your equity vs villain's range: <b>${pct(s.equity)}</b> (needed ${pct(s.required)}).</p>` +
    `<p>Best play: <b>${s.should_call ? 'CALL' : 'FOLD'}</b> (call EV ${s.call_ev >= 0 ? '+' : ''}${s.call_ev.toFixed(2)}bb). ` +
    `You said ${called ? 'call' : 'fold'} → <span class="${right ? 'verdict-good' : 'verdict-bad'}">${right ? 'correct' : 'wrong'}</span></p>`;
}
$('po-call').onclick = () => poAnswer(true);
$('po-fold').onclick = () => poAnswer(false);

// ---- pot-odds spot (JS forward-sim; equity vs range via wasm equity_vs_reach) ----
// Each spot is drawn from the selected HU ruleset's solved preflop equilibrium.
// This mirrors `drill pot-odds --preflop <hu>` (trainer.rs) — the sim reuses the
// same chart nodes the browser below walks, and wasm scores hero's equity
// against the villain seat's whole range (its per-class reach), not one hand.
const BET_FRACTIONS = [0.33, 0.5, 0.75, 1.0, 1.5];

// Grid index of a two-card holding — matches preflop::class_index (RANKS below,
// defined for the grid): suited ? hi*13+lo : lo*13+hi, hi=min rank, lo=max.
const clsIdx = (a, b) => {
  const x = RANKS.indexOf(a[0]), y = RANKS.indexOf(b[0]);
  const hi = Math.min(x, y), lo = Math.max(x, y);
  return a[1] === b[1] ? hi * 13 + lo : lo * 13 + hi;
};

// Forward-simulate one HU preflop hand to a flop; null on a dead line (fold /
// all-in) or a pruned node — caller retries. Port of sample_preflop_flop_spot.
function samplePreflopSpot() {
  const deck = [];
  for (const r of RANKS) for (const s of 'shdc') deck.push(r + s);
  for (let i = deck.length - 1; i > 0; i--) {   // Fisher–Yates
    const j = Math.floor(Math.random() * (i + 1));
    [deck[i], deck[j]] = [deck[j], deck[i]];
  }
  const sb = [deck[0], deck[1]], bb = [deck[2], deck[3]];
  const sbCls = clsIdx(sb[0], sb[1]), bbCls = clsIdx(bb[0], bb[1]);
  // Each seat's per-class arrival probability along the line (mirrors pfReach),
  // accumulated as we walk — the villain seat's is scored against.
  const reach = { SB: new Float32Array(169).fill(1), BB: new Float32Array(169).fill(1) };

  let path = '';
  const line = [];
  for (;;) {
    const node = poNodes[path];
    if (!node) return null;                       // pruned/missing => abort
    const cls = node.seat === 'SB' ? sbCls : bbCls;
    const w = node.freqs.map(f => f[cls]);
    const wsum = w.reduce((a, b) => a + b, 0);
    if (wsum <= 0) return null;                    // this class never arrives here
    let roll = Math.random() * wsum, ai = w.length - 1;
    for (let i = 0; i < w.length; i++) { roll -= w[i]; if (roll < 0) { ai = i; break; } }
    const label = node.actions[ai];
    line.push(`${node.seat} ${pfVerb(label)}`);
    // Fold this action into the acting seat's reach over all 169 classes.
    const fr = node.freqs[ai], rs = reach[node.seat];
    for (let c = 0; c < 169; c++) rs[c] *= fr[c];

    // hu_step: fold/all-in dead; check or a called raise opens a flop; a call at
    // the root is the SB limp (BB still acts); anything else continues.
    if (label === 'Fold' || label === 'All-in') return null;
    let pot = null;
    if (label === 'Check') pot = node.pot_bb;
    else if (label === 'Call' && path !== '') pot = node.pot_bb + node.to_call_bb;
    if (pot !== null) {
      // ponytail: starter-tier only on the deployed site; deep lines self-prune
      // via retry. Coarse all-in guard — deep HU sets (cash-hu89) don't hit it.
      if (poStack && pot >= 2 * poStack) return null;
      return { sb, bb, flop: [deck[4], deck[5], deck[6]], pot, line, sbReach: reach.SB, bbReach: reach.BB };
    }
    path = path === '' ? pfTok(label) : `${path}-${pfTok(label)}`; // SB limp or a raise
  }
}

// A fully scored spot (hero hand, flop, pot, bet, and equity vs villain's
// range) plus a preflop `line`, or null if 200 sims never reached a bettable
// flop. Hero is a random seat; villain's range is the other seat's reach.
function preflopPotOddsSpot() {
  let s = null;
  for (let i = 0; i < 200 && !s; i++) s = samplePreflopSpot();
  if (!s) return null;
  const heroIsSb = Math.random() < 0.5;
  const hero = heroIsSb ? s.sb : s.bb;
  const villainReach = heroIsSb ? s.bbReach : s.sbReach;
  const pot = s.pot;
  const bet = pot * BET_FRACTIONS[Math.floor(Math.random() * BET_FRACTIONS.length)];
  const required = bet / (pot + 2 * bet);
  const equity = equity_vs_reach(hero.join(''), s.flop.join(''), villainReach);
  return {
    hero, flop: s.flop, pot, bet, required, equity,
    should_call: equity >= required,
    call_ev: equity * (pot + bet) - (1 - equity) * bet,
    line: s.line,
  };
}

// HU ruleset ids, grouped by family (asc) then shallow->deep — shared by the
// pot-odds drill and the flop equity explorer.
function huIds(ids) {
  const depth = id => +id.match(/(\d+)$/)[1];
  const family = id => id.replace(/-?\d+$/, '');
  return ids.filter(id => id.includes('-hu'))
    .sort((a, b) => family(a) === family(b) ? depth(a) - depth(b)
      : family(a) < family(b) ? -1 : 1);
}

// Fetch one HU ruleset's header + starter-tier nodes (path -> record).
async function loadHuTable(id) {
  const [header, lines] = await Promise.all([
    fetch(`preflop/${id}/header.json`).then(r => r.json()),
    fetch(`preflop/${id}/starter.jsonl`).then(r => r.text()),
  ]);
  const nodes = {};
  for (const l of lines.split('\n')) if (l.trim()) { const n = JSON.parse(l); nodes[n.path] = n; }
  return { header, nodes, stack: header.config.stack_bb ?? null };
}

async function poInit() {
  let ids;
  try { ids = await (await fetch('preflop/index.json')).json(); }
  catch {   // charts not staged: the drill needs them, so say so
    $('po-spot').innerHTML = '<p>Preflop charts aren’t staged — run the data build to enable this drill.</p>';
    return;
  }
  const hu = huIds(ids);
  if (!hu.length) return;
  const sel = $('po-source');
  sel.innerHTML = hu.map(id => `<option value="${id}">${id}</option>`).join('');
  const loadSource = async () => {
    poSource = sel.value;
    poSpot = null;
    poNodes = null;   // block Deal until the new set is in
    $('po-actions').hidden = true;
    $('po-spot').innerHTML = '';
    $('po-reveal').innerHTML = '';
    const t = await loadHuTable(poSource);
    poHeader = t.header;
    poNodes = t.nodes;
    poStack = t.stack;
  };
  sel.onchange = loadSource;
  // Default to cash-hu89 when present, else the shallowest HU set; load it now.
  sel.value = hu.includes('cash-hu89') ? 'cash-hu89' : hu[0];
  await loadSource();
}

// ---- flop equity explorer (JS line walk; equity vs range via wasm) ----------
// Pick a HU preflop line (fixes both arrival ranges), enter your hand + a board
// (flop through river), and score your equity vs villain's whole range. Postflop
// the BB is OOP and the SB (button) is IP.

let feNodes = null, feStack = null, feLines = [];

// Both seats' per-class arrival reach along a fixed line (token path), the JS
// mirror of PreflopCharts::class_reach for two seats at once. null if a token
// isn't in the committed starter tier.
function reachForLine(tokens) {
  const reach = { SB: new Float32Array(169).fill(1), BB: new Float32Array(169).fill(1) };
  let prefix = '';
  for (const tok of tokens) {
    const node = feNodes[prefix];
    if (!node) return null;
    const ai = node.actions.findIndex(l => pfTok(l) === tok);
    if (ai < 0) return null;
    const rs = reach[node.seat], fr = node.freqs[ai];
    for (let c = 0; c < 169; c++) rs[c] *= fr[c];
    prefix = prefix ? `${prefix}-${tok}` : tok;
  }
  return reach;
}

// Every action line that closes to a flop (check-through or a called raise) as
// {tokens, label, pot}. Fold/all-in lines and pruned nodes drop out; the pot is
// what's in the middle when the flop is dealt.
function enumerateFlopLines(nodes) {
  const out = [];
  const walk = (path, tokens, labels) => {
    const node = nodes[path];
    if (!node) return;   // pruned/missing
    node.actions.forEach((label) => {
      const tok = pfTok(label);
      const nextTokens = [...tokens, tok];
      const nextLabels = [...labels, `${node.seat} ${pfVerb(label)}`];
      if (label === 'Fold' || label === 'All-in') return;   // dead line
      // A check, or a called raise past the root (a root call is the SB limp —
      // BB still acts), opens the flop. Mirrors the pot-odds sampler.
      let pot = null;
      if (label === 'Check') pot = node.pot_bb;
      else if (label === 'Call' && path !== '') pot = node.pot_bb + node.to_call_bb;
      if (pot !== null) {
        if (feStack && pot >= 2 * feStack) return;   // coarse all-in guard
        out.push({ tokens: nextTokens, label: nextLabels.join(', '), pot });
        return;   // flop reached — stop this branch
      }
      walk(path ? `${path}-${tok}` : tok, nextTokens, nextLabels);
    });
  };
  walk('', [], []);
  return out;
}

$('fe-run').onclick = () => {
  const out = $('fe-out');
  if (!feNodes) { out.textContent = 'Charts still loading…'; return; }
  const li = feLines[+$('fe-line').value];
  if (!li) { out.textContent = 'No flop-closing line for this table — pick another.'; return; }
  const hero = $('fe-hero').value.trim();
  const board = $('fe-board').value.trim().replace(/\s+/g, '');
  const heroOop = $('fe-pos').value === 'oop';
  const heroList = hero.match(/.{2}/g) || [], boardList = board.match(/.{2}/g) || [];
  const dup = heroList.find(c => boardList.includes(c));
  if (dup) { out.textContent = `Your ${dup} is also on the board.`; return; }
  const reach = reachForLine(li.tokens);
  if (!reach) { out.textContent = 'This line isn’t in the committed starter charts.'; return; }
  const villainReach = heroOop ? reach.SB : reach.BB;   // OOP=BB, so villain=IP=SB
  out.textContent = 'Computing…';
  setTimeout(() => {   // let the label paint before the synchronous MC loop
    try {
      const equity = equity_vs_reach(hero, board, villainReach);
      const pot = li.pot;
      const bet = parseFloat($('fe-bet').value) || 0;
      let verdict = '';
      if (bet > 0) {
        const required = bet / (pot + 2 * bet);
        const callEv = equity * (pot + bet) - (1 - equity) * bet;
        verdict = `<p>Facing <b>${bet.toFixed(1)}bb</b> into ${pot.toFixed(1)}bb: you need <b>${pct(required)}</b> → ` +
          `<b>${equity >= required ? 'CALL' : 'FOLD'}</b> (call EV ${callEv >= 0 ? '+' : ''}${callEv.toFixed(2)}bb).</p>`;
      }
      const n = boardList.length;
      const bucket = n === 3 ? ` · you have <b>${made_hand(hero, board)}</b>` : '';
      out.innerHTML =
        `<p class="spotline">Hero ${cardsHTML(heroList)} (${heroOop ? 'OOP / BB' : 'IP / SB'}) · ` +
        `Board ${cardsHTML(boardList)} · Pot ${pot.toFixed(1)}bb${bucket}</p>` +
        `<p>Equity vs villain's whole range: <b>${pct(equity)}</b> ` +
        (n === 5 ? '<small>(exact — full board)</small>' : `<small>(Monte-Carlo, ${5 - n}-card runout)</small>`) + '</p>' +
        verdict;
    } catch (e) {
      out.textContent = 'Error: ' + (e.message || e);
    }
  }, 20);
};

async function feInit() {
  let ids;
  try { ids = await (await fetch('preflop/index.json')).json(); }
  catch {
    $('fe-out').innerHTML = '<p>Preflop charts aren’t staged — run the data build to enable this.</p>';
    return;
  }
  const hu = huIds(ids);
  if (!hu.length) return;
  const sel = $('fe-source');
  sel.innerHTML = hu.map(id => `<option value="${id}">${id}</option>`).join('');
  const loadSource = async () => {
    const t = await loadHuTable(sel.value);
    feNodes = t.nodes;
    feStack = t.stack;
    feLines = enumerateFlopLines(feNodes);
    $('fe-line').innerHTML = feLines.map((l, i) =>
      `<option value="${i}">${l.label} — pot ${l.pot.toFixed(1)}bb</option>`).join('');
    $('fe-out').innerHTML = '';
  };
  sel.onchange = loadSource;
  sel.value = hu.includes('cash-hu89') ? 'cash-hu89' : hu[0];
  await loadSource();
}

// ---- preflop chart browser (fetches data/preflop/, no wasm) ------------------

let pfNodes = null;   // path -> starter-tier node record
let pfHeader = null;
let pfPath = [];      // action tokens from the root

const pfTok = l => l === 'Fold' ? 'f' : l === 'Call' ? 'c' : l === 'All-in' ? 'ai'
  : 'r' + l.replace('Raise to ', '').replace('bb', '');
const pfVerb = l => l === 'Fold' ? 'folds' : l === 'Call' ? 'calls' : l === 'Check' ? 'checks'
  : l === 'All-in' ? 'jams' : l.toLowerCase().replace('raise', 'raises');

async function pfLoad(id) {
  const [header, lines] = await Promise.all([
    fetch(`preflop/${id}/header.json`).then(r => r.json()),
    fetch(`preflop/${id}/starter.jsonl`).then(r => r.text()),
  ]);
  pfHeader = header;
  pfNodes = {};
  for (const l of lines.split('\n')) if (l.trim()) { const n = JSON.parse(l); pfNodes[n.path] = n; }
  pfPath = [];
  pfRender();
}

// The acting seat's per-class arrival probability: the product of its own
// past action frequencies along the line (all ancestors are stored).
function pfReach(node) {
  const reach = new Float32Array(169).fill(1);
  let prefix = '';
  for (const tok of pfPath) {
    const anc = pfNodes[prefix];
    if (anc && anc.seat === node.seat) {
      const ai = anc.actions.findIndex(l => pfTok(l) === tok);
      if (ai >= 0) for (let c = 0; c < 169; c++) reach[c] *= anc.freqs[ai][c];
    }
    prefix = prefix ? prefix + '-' + tok : tok;
  }
  return reach;
}

function pfRender() {
  const node = pfNodes[pfPath.join('-')];

  // Breadcrumb: clicking a crumb truncates the line back to it.
  const crumbs = ['<button class="crumb" data-i="0">start</button>'];
  let prefix = '';
  pfPath.forEach((tok, i) => {
    const anc = pfNodes[prefix];
    const ai = anc ? anc.actions.findIndex(l => pfTok(l) === tok) : -1;
    const text = ai >= 0 ? `${anc.seat} ${pfVerb(anc.actions[ai])}` : tok;
    crumbs.push(`<button class="crumb" data-i="${i + 1}">${text}</button>`);
    prefix = prefix ? prefix + '-' + tok : tok;
  });
  $('pf-crumbs').innerHTML = crumbs.join(' › ');
  document.querySelectorAll('#pf-crumbs .crumb').forEach(b =>
    b.onclick = () => { pfPath = pfPath.slice(0, +b.dataset.i); pfRender(); });

  if (!node) {
    $('pf-head').innerHTML = '<b>Line not stored</b><div class="sub">below the committed ' +
      'starter reach — regenerate the full charts.jsonl locally for more depth (design 07)</div>';
    for (const id of ['pf-actions', 'pf-legend', 'pf-grid', 'pf-detail']) $(id).innerHTML = '';
    return;
  }

  $('pf-head').innerHTML =
    `<b>${pfHeader.label}</b><div class="sub">${node.seat} to act · pot ${node.pot_bb}bb · ` +
    `${node.to_call_bb}bb to call · spot reach ${(node.reach * 100).toFixed(1)}%</div>`;
  $('pf-actions').innerHTML = node.actions.map((a, i) =>
    `<button class="answer" data-i="${i}">${a}</button>`).join(' ');
  document.querySelectorAll('#pf-actions button').forEach(b =>
    b.onclick = () => { pfPath.push(pfTok(node.actions[+b.dataset.i])); pfRender(); });
  $('pf-legend').innerHTML = node.actions.map(a =>
    `<span><span class="chip" style="background:${actionColor(a, 0, node.actions)}"></span>${a}</span>`).join('');

  const reach = pfReach(node);
  const cells = [];
  for (let i = 0; i < 13; i++) for (let j = 0; j < 13; j++) {
    const name = i === j ? RANKS[i] + RANKS[j]
      : i < j ? RANKS[i] + RANKS[j] + 's' : RANKS[j] + RANKS[i] + 'o';
    const c = i * 13 + j;
    if (reach[c] < 1e-4) { cells.push(`<div class="cell dead">${name}</div>`); continue; }
    let at = 0;
    const stops = node.actions.map((a, k) => {
      const f = node.freqs[k][c];
      const seg = `${actionColor(a, k, node.actions)} ${at * 100}% ${(at + f) * 100}%`;
      at += f;
      return seg;
    }).join(', ');
    cells.push(`<div class="cell" data-c="${c}" data-name="${name}" ` +
      `style="background:linear-gradient(to left, ${stops})">${name}</div>`);
  }
  $('pf-grid').innerHTML = cells.join('');
  $('pf-detail').innerHTML = '<p class="sub" style="color:var(--muted)">Click a cell for frequencies and EV.</p>';
  document.querySelectorAll('#pf-grid .cell[data-c]').forEach(el => el.onclick = () => pfDetail(node, el));
}

function pfDetail(node, cell) {
  document.querySelectorAll('#pf-grid .cell.sel').forEach(c => c.classList.remove('sel'));
  cell.classList.add('sel');
  const c = +cell.dataset.c;
  const evs = node.evs;
  const best = evs ? evs.reduce((b, e, k) => (e[c] > evs[b][c] ? k : b), 0) : -1;
  const head = '<tr><th></th>' + node.actions.map(a => `<th>${a}</th>`).join('') + '</tr>';
  const row = `<tr><td>${cell.dataset.name}</td>` + node.actions.map((_, k) =>
    `<td class="${k === best ? 'best' : ''}">${(node.freqs[k][c] * 100).toFixed(0)}%` +
    (evs ? ` <small>(${evs[k][c].toFixed(2)})</small>` : '') + '</td>').join('') + '</tr>';
  $('pf-detail').innerHTML = `<table>${head}${row}</table>` +
    `<p class="sub" style="color:var(--muted)">Frequency <small>(EV in ${pfHeader.ev_unit})</small>; green = highest EV.</p>`;
}

async function pfInit() {
  let ids;
  try {
    ids = await (await fetch('preflop/index.json')).json();
  } catch {
    $('pf-head').textContent = 'Preflop charts not staged — see web/README for the local copy step.';
    return;
  }
  // ids encode family + depth: "cash89" -> ["cash", 89], "cash-hu89" -> ["cash-hu", 89].
  const parse = id => { const m = id.match(/^(.*?)-?(\d+)$/); return m ? [m[1], +m[2]] : [id, 0]; };
  // Table display names: bare cash/mtt are 6-max, the -hu families are heads-up.
  const FAMILY_LABEL = { cash: 'Cash 6-max', 'cash-hu': 'Cash HU', mtt: 'MTT 6-max', 'mtt-hu': 'MTT HU' };
  const label = f => FAMILY_LABEL[f] || f.replace(/-/g, ' ').replace(/\b\w/g, c => c.toUpperCase());

  // Two cascading selects: Table (family, asc) -> Depth (blind level, desc).
  const fams = {};
  for (const id of ids) { const [fam] = parse(id); (fams[fam] ||= []).push(id); }
  const famNames = Object.keys(fams).sort();
  for (const f of famNames) fams[f].sort((a, b) => parse(b)[1] - parse(a)[1]);
  $('pf-family').innerHTML = famNames.map(f => `<option value="${f}">${label(f)}</option>`).join('');

  const fillDepths = fam =>
    $('pf-depth').innerHTML = fams[fam].map(id => `<option value="${id}">${parse(id)[1]} BB</option>`).join('');
  $('pf-family').onchange = () => { fillDepths($('pf-family').value); pfLoad($('pf-depth').value); };
  $('pf-depth').onchange = () => pfLoad($('pf-depth').value);

  fillDepths(famNames[0]);
  await pfLoad($('pf-depth').value);
}

// ---- GTO strategy grid ------------------------------------------------------

const RANKS = 'AKQJT98765432';
const NODE_LABELS = {
  'ip': 'IP: BB checks — c-bet?',
  'oop-33': 'OOP: facing 33% c-bet',
  'oop-75': 'OOP: facing 75% c-bet',
};
let grFiles = {};   // flop -> node -> filename

function actionColor(label, i, actions) {
  if (/^fold/i.test(label)) return 'var(--act-fold)';
  if (/^(check|call)/i.test(label)) return 'var(--act-passive)';
  if (/^all-?in/i.test(label)) return 'var(--act-allin)';
  // aggressive: darker red for the larger sizes, by position among bets/raises
  const aggr = actions.filter(a => !/^(fold|check|call)/i.test(a));
  const k = aggr.indexOf(label);
  return ['var(--act-bet1)', 'var(--act-bet2)', 'var(--act-bet3)'][Math.min(k, 2)];
}

function comboClass(hand) {
  const [r1, s1, r2, s2] = hand;
  if (r1 === r2) return r1 + r2;
  const [hi, lo] = RANKS.indexOf(r1) < RANKS.indexOf(r2) ? [r1, r2] : [r2, r1];
  return hi + lo + (s1 === s2 ? 's' : 'o');
}

// Render a SolvedSpot-shaped `spot` into the `prefix`-{head,legend,grid,detail}
// elements. Shared by the GTO grid (prefix 'gr') and the tables viewer ('tb').
function renderGrid(spot, prefix) {
  const actions = spot.strategies[0].strategy.actions;
  $(prefix + '-head').innerHTML =
    `<b>${spot.label}</b><div class="sub">${spot.villain_action} · pot ${spot.pot_bb}bb` +
    (spot.generator ? ` · exploitability ${spot.generator.exploitability_bb.toFixed(3)}bb` : '') + `</div>`;
  $(prefix + '-legend').innerHTML = actions.map(a =>
    `<span><span class="chip" style="background:${actionColor(a, 0, actions)}"></span>${a}</span>`).join('');

  const classes = {};   // class name -> [strategy entries]
  for (const s of spot.strategies) (classes[comboClass(s.hand)] ??= []).push(s);

  const cells = [];
  for (let i = 0; i < 13; i++) for (let j = 0; j < 13; j++) {
    const name = i === j ? RANKS[i] + RANKS[j]
      : i < j ? RANKS[i] + RANKS[j] + 's' : RANKS[j] + RANKS[i] + 'o';
    const combos = classes[name];
    if (!combos) { cells.push(`<div class="cell dead">${name}</div>`); continue; }
    // mean frequency per action over the class's combos → gradient segments
    const means = actions.map((_, k) =>
      combos.reduce((t, c) => t + c.strategy.frequencies[k], 0) / combos.length);
    let at = 0;
    const stops = means.map((f, k) => {
      const seg = `${actionColor(actions[k], k, actions)} ${at * 100}% ${(at + f) * 100}%`;
      at += f;
      return seg;
    }).join(', ');
    cells.push(`<div class="cell" data-class="${name}" style="background:linear-gradient(to left, ${stops})">${name}</div>`);
  }
  $(prefix + '-grid').innerHTML = cells.join('');
  $(prefix + '-detail').innerHTML = '<p class="sub" style="color:var(--muted)">Click a cell for the per-combo breakdown.</p>';
  document.querySelectorAll(`#${prefix}-grid .cell[data-class]`).forEach(c =>
    c.onclick = () => renderDetail(spot, classes, prefix, c));
}

function renderDetail(spot, classes, prefix, cell) {
  document.querySelectorAll(`#${prefix}-grid .cell.sel`).forEach(c => c.classList.remove('sel'));
  cell.classList.add('sel');
  const combos = classes[cell.dataset.class];
  const actions = spot.strategies[0].strategy.actions;
  const head = '<tr><th>combo</th>' + actions.map(a => `<th>${a}</th>`).join('') + '</tr>';
  const rows = combos.map(c => {
    const best = c.strategy.action_ev.indexOf(Math.max(...c.strategy.action_ev));
    const tds = actions.map((_, k) =>
      `<td class="${k === best ? 'best' : ''}">${(c.strategy.frequencies[k] * 100).toFixed(0)}% ` +
      `<small>(${c.strategy.action_ev[k].toFixed(2)})</small></td>`).join('');
    return `<tr><td>${cardsHTML(c.hand.match(/.{2}/g))}</td>${tds}</tr>`;
  }).join('');
  $(prefix + '-detail').innerHTML =
    `<table>${head}${rows}</table>` +
    `<p class="sub" style="color:var(--muted)">Cells: frequency <small>(EV in bb)</small>; green = highest-EV action.</p>`;
}

async function grLoad() {
  const flop = $('gr-flop').value;
  const node = $('gr-node').value;
  const file = grFiles[flop]?.[node];
  if (!file) return;
  renderGrid(await (await fetch('solutions/' + file)).json(), 'gr');
}

function grFillNodes() {
  const nodes = Object.keys(grFiles[$('gr-flop').value] || {});
  $('gr-node').innerHTML = nodes.map(n =>
    `<option value="${n}">${NODE_LABELS[n] || n}</option>`).join('');
}

async function grInit() {
  let names;
  try {
    names = await (await fetch('solutions/index.json')).json();
  } catch {
    $('gr-head').textContent = 'Solution files not staged — see web/README for the local copy step.';
    return;
  }
  for (const f of names) {
    const m = f.match(/^([2-9TJQKA][cdhs]{1}[2-9TJQKA][cdhs][2-9TJQKA][cdhs])-([0-9a-f]{8})-(.+)\.json$/);
    if (!m) continue;
    (grFiles[m[1]] ??= {})[m[3]] = f;
  }
  $('gr-flop').innerHTML = Object.keys(grFiles).sort().map(f =>
    `<option value="${f}">${f.match(/.{2}/g).join(' ')}</option>`).join('');
  $('gr-flop').onchange = () => { grFillNodes(); grLoad(); };
  $('gr-node').onchange = grLoad;
  grFillNodes();
  grLoad();
}

// ---- Reach-pruned tables (flop grids across all formations) -----------------
// Same 13×13 grid as above, fed by the committed `data/tables-web/` export
// (`poker-trainer export-tables-web`): every formation, 25 flops, each flop's
// flop-decision nodes. The lean export hoists `actions` to the node and stores
// per-combo `freqs`/`evs`; tbReshape re-nests them into renderGrid's shape.

const TB_FORMATION_LABELS = {
  'srp-btn-bb': 'SRP BTN vs BB', 'srp-co-bb': 'SRP CO vs BB', 'srp-sb-bb': 'SRP SB vs BB',
  '3bp-bb-btn': '3-bet pot BB vs BTN', '3bp-btn-co': '3-bet pot BTN vs CO',
};
let tbIndex = null;   // formation -> {hash, flops:[{stem,display}]}
let tbNodes = [];     // current flop's reshaped node-spots

function tbReshape(n) {
  n.strategies = n.strategies.map(s =>
    ({ hand: s.hand, strategy: { actions: n.actions, frequencies: s.freqs, action_ev: s.evs } }));
  return n;
}

function tbNodeLabel(n) {
  return `${n.hero_oop ? 'OOP' : 'IP'}: ${n.line.length ? n.line.join(' · ') : 'first decision'}`;
}

async function tbLoad() {
  const f = $('tb-formation').value, stem = $('tb-flop').value, hash = tbIndex[f].hash;
  const text = await (await fetch(`tables/${f}/${stem}-${hash}.jsonl`)).text();
  tbNodes = text.split('\n').filter(l => l.trim()).map(l => tbReshape(JSON.parse(l)));
  $('tb-node').innerHTML = tbNodes.map((n, i) => `<option value="${i}">${tbNodeLabel(n)}</option>`).join('');
  tbShow();
}

function tbShow() {
  renderGrid(tbNodes[+$('tb-node').value], 'tb');
}

function tbFillFlops() {
  $('tb-flop').innerHTML = tbIndex[$('tb-formation').value].flops.map(fl =>
    `<option value="${fl.stem}">${fl.display}</option>`).join('');
  tbLoad();
}

async function tbInit() {
  try { tbIndex = await (await fetch('tables/index.json')).json(); }
  catch { $('tb-head').textContent = 'Table exports not staged — run `poker-trainer export-tables-web` (see web/README).'; return; }
  $('tb-formation').innerHTML = Object.keys(tbIndex).sort().map(f =>
    `<option value="${f}">${TB_FORMATION_LABELS[f] || f}</option>`).join('');
  $('tb-formation').onchange = tbFillFlops;
  $('tb-flop').onchange = tbLoad;
  $('tb-node').onchange = tbShow;
  tbFillFlops();
}

// ---- boot -------------------------------------------------------------------

await init();
poInit();
feInit();
pfInit();
grInit();
tbInit();
