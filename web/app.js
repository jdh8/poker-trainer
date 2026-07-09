// Framework-free glue: wasm exports return JSON strings, we parse and render.
// The GTO grid and preflop chart sections never touch wasm — they fetch the
// committed JSON directly.
import init, { equity_report, deal_pot_odds } from './pkg/poker_trainer_web.js';

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

$('po-deal').onclick = () => {
  poSpot = JSON.parse(deal_pot_odds());
  $('po-spot').innerHTML =
    `<p class="spotline">Your hand: ${cardsHTML(poSpot.hero)} &nbsp; Flop: ${cardsHTML(poSpot.flop)}</p>` +
    `<p>Pot ${poSpot.pot.toFixed(0)}bb. Villain bets <b>${poSpot.bet.toFixed(1)}bb</b> — ` +
    `call ${poSpot.bet.toFixed(1)} to win ${(poSpot.pot + poSpot.bet).toFixed(1)}, ` +
    `so you need <b>${pct(poSpot.required)}</b> equity.</p>`;
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
    `<p>Villain had ${cardsHTML(s.villain)} — your true equity ${pct(s.equity)} (needed ${pct(s.required)}).</p>` +
    `<p>Best play: <b>${s.should_call ? 'CALL' : 'FOLD'}</b> (call EV ${s.call_ev >= 0 ? '+' : ''}${s.call_ev.toFixed(2)}bb). ` +
    `You said ${called ? 'call' : 'fold'} → <span class="${right ? 'verdict-good' : 'verdict-bad'}">${right ? 'correct' : 'wrong'}</span></p>`;
}
$('po-call').onclick = () => poAnswer(true);
$('po-fold').onclick = () => poAnswer(false);

// ---- preflop chart browser (fetches data/preflop/, no wasm) ------------------

let pfNodes = null;   // path -> starter-tier node record
let pfHeader = null;
let pfPath = [];      // action tokens from the root

const pfTok = l => l === 'Fold' ? 'f' : l === 'Call' ? 'c' : l === 'All-in' ? 'ai'
  : 'r' + l.replace('Raise to ', '').replace('bb', '');
const pfVerb = l => l === 'Fold' ? 'folds' : l === 'Call' ? 'calls' : l === 'All-in' ? 'jams'
  : l.toLowerCase().replace('raise', 'raises');

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
let grSpot = null;  // fetched solution
let grClasses = {}; // class name -> [strategy entries]

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

function renderGrid() {
  const actions = grSpot.strategies[0].strategy.actions;
  $('gr-head').innerHTML =
    `<b>${grSpot.label}</b><div class="sub">${grSpot.villain_action} · pot ${grSpot.pot_bb}bb · ` +
    `exploitability ${grSpot.generator.exploitability_bb.toFixed(3)}bb</div>`;
  $('gr-legend').innerHTML = actions.map(a =>
    `<span><span class="chip" style="background:${actionColor(a, 0, actions)}"></span>${a}</span>`).join('');

  grClasses = {};
  for (const s of grSpot.strategies) (grClasses[comboClass(s.hand)] ??= []).push(s);

  const cells = [];
  for (let i = 0; i < 13; i++) for (let j = 0; j < 13; j++) {
    const name = i === j ? RANKS[i] + RANKS[j]
      : i < j ? RANKS[i] + RANKS[j] + 's' : RANKS[j] + RANKS[i] + 'o';
    const combos = grClasses[name];
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
  $('gr-grid').innerHTML = cells.join('');
  $('gr-detail').innerHTML = '<p class="sub" style="color:var(--muted)">Click a cell for the per-combo breakdown.</p>';
  document.querySelectorAll('#gr-grid .cell[data-class]').forEach(c => c.onclick = () => renderDetail(c));
}

function renderDetail(cell) {
  document.querySelectorAll('#gr-grid .cell.sel').forEach(c => c.classList.remove('sel'));
  cell.classList.add('sel');
  const combos = grClasses[cell.dataset.class];
  const actions = grSpot.strategies[0].strategy.actions;
  const head = '<tr><th>combo</th>' + actions.map(a => `<th>${a}</th>`).join('') + '</tr>';
  const rows = combos.map(c => {
    const best = c.strategy.action_ev.indexOf(Math.max(...c.strategy.action_ev));
    const tds = actions.map((_, k) =>
      `<td class="${k === best ? 'best' : ''}">${(c.strategy.frequencies[k] * 100).toFixed(0)}% ` +
      `<small>(${c.strategy.action_ev[k].toFixed(2)})</small></td>`).join('');
    return `<tr><td>${cardsHTML(c.hand.match(/.{2}/g))}</td>${tds}</tr>`;
  }).join('');
  $('gr-detail').innerHTML =
    `<table>${head}${rows}</table>` +
    `<p class="sub" style="color:var(--muted)">Cells: frequency <small>(EV in bb)</small>; green = highest-EV action.</p>`;
}

async function grLoad() {
  const flop = $('gr-flop').value;
  const node = $('gr-node').value;
  const file = grFiles[flop]?.[node];
  if (!file) return;
  grSpot = await (await fetch('solutions/' + file)).json();
  renderGrid();
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

// ---- boot -------------------------------------------------------------------

await init();
pfInit();
grInit();
