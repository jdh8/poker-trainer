// Framework-free glue: wasm exports return JSON strings, we parse and render.
// The GTO grid section never touches wasm — it fetches solution JSON directly.
import init, { equity_report, deal_pot_odds, deal_texture } from './pkg/poker_trainer_web.js';

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

// ---- texture drill ----------------------------------------------------------

let txSpot = null, txRight = 0, txTotal = 0, txPick = {};

$('tx-deal').onclick = () => {
  txSpot = JSON.parse(deal_texture());
  txPick = {};
  $('tx-spot').innerHTML = `<p class="spotline">Flop: ${cardsHTML(txSpot.flop)}</p>`;
  $('tx-questions').hidden = false;
  $('tx-reveal').innerHTML = '';
  document.querySelectorAll('#tx-questions button').forEach(b => b.classList.remove('picked'));
};

document.querySelectorAll('#tx-suits button').forEach(b => b.onclick = () => txChoose('suits', b));
document.querySelectorAll('#tx-paired button').forEach(b => b.onclick = () => txChoose('paired', b));

function txChoose(kind, btn) {
  if (!txSpot || kind in txPick) return;
  txPick[kind] = btn.dataset.v;
  btn.classList.add('picked');
  if (!('suits' in txPick) || !('paired' in txPick)) return;
  const s = txSpot; txSpot = null;
  const right = txPick.suits === s.suits && (txPick.paired === 'true') === s.paired;
  txTotal++; if (right) txRight++;
  $('tx-score').textContent = `${txRight}/${txTotal} correct`;
  $('tx-reveal').innerHTML =
    `<p>Texture: <b>${s.suits}</b>, <b>${s.paired ? 'paired' : 'unpaired'}</b>, ` +
    `${s.straighty ? 'straighty' : 'disconnected'}, high card ${s.high} ` +
    `→ <span class="${right ? 'verdict-good' : 'verdict-bad'}">${right ? 'correct' : 'wrong'}</span></p>`;
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
    cells.push(`<div class="cell" data-class="${name}" style="background:linear-gradient(to right, ${stops})">${name}</div>`);
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
grInit();
