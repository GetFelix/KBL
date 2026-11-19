// Drives the built dashboard (dist/) in headless Chrome against a fake gateway, then
// checks keyboard flows and runs axe-core in both themes. Exits non-zero on a failed
// flow or any serious or critical axe violation.
// Usage: node check/browser.mjs [screenshot-dir]   (after `npm run build`)
import { mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { createRequire } from 'node:module';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { launch, serve } from '../../scripts/lib/browser.mjs';
import { FOLD_BUDGET_MS, ITEMS, PAINT_BUDGET_MS } from './budget.mjs';
import { bigLog, fixtureLog } from './fixture.mjs';

const shots = process.argv[2];
if (shots) mkdirSync(shots, { recursive: true });
const axeSource = readFileSync(createRequire(import.meta.url).resolve('axe-core/axe.min.js'), 'utf8');

// Replaces WebSocket before the page's code runs: answers `join`, pages `read`s of the
// fixture log, and `subscribe`s from the cursor. `mode` picks the gateway's behaviour.
// `window.__push(record)` delivers one more record as a live event. A throw in the page's
// handler is left to surface as a page exception, which fails the check.
const encode = (r) => btoa(unescape(encodeURIComponent(JSON.stringify(r))));
const fakeGateway = (log, mode, pageSize = 20) => `(() => {
  const encode = ${encode};
  const log = ${JSON.stringify(log.map(encode))};
  const mode = ${JSON.stringify(mode)};
  window.WebSocket = class {
    constructor(url) {
      (window.__urls ||= []).push(url);
      window.__ws = this;
      setTimeout(() => (mode === 'down' ? this.onclose && this.onclose() : this.onopen && this.onopen()), 0);
    }
    reply(m, ms = 0) { setTimeout(() => this.onmessage && this.onmessage({ data: JSON.stringify(m) }), ms); }
    send(text) {
      const m = JSON.parse(text);
      if (m.type === 'join') return mode === 'forbidden'
        ? this.reply({ type: 'error', code: 'signed_out', message: 'the ID token expired' })
        : this.reply({ type: 'hello', features: ['read'] });
      if (m.type === 'read') {
        if (mode === 'loading') return;
        if (mode === 'trimmed') return this.reply({ type: 'error', code: 'trimmed', message: 'the log no longer holds offset 0' });
        const records = log.slice(m.from, m.from + ${pageSize}).map((payload, i) => ({ offset: m.from + i, payload }));
        return this.reply({ type: 'records', id: m.id, records, next_offset: m.from + records.length });
      }
      if (m.type === 'subscribe') {
        this.liveAt = m.from;
        this.reply({ type: 'subscribed', start_offset: m.from, live_offset: m.from });
      }
    }
    close() {}
  };
  // A fake tracker at the page's own origin: person routes append the record they name
  // to the fake log, the way the real tracker does, so the result comes back live.
  const ROUTES = {
    '/v1/work/proposals/accept': (b) => ({ kind: 'accepted', item: b.item, tier: b.tier }),
    '/v1/work/proposals/reject': (b) => ({ kind: 'rejected', item: b.item }),
    '/v1/work/prices': (b) => ({ v: 8, kind: 'price_set', model: b.model, unit: b.unit, price_micros: b.price_micros, effective_from_ms: b.effective_from_ms }),
    '/v1/work/proposals/edit': (b) => ({ v: 17, kind: 'edited', item: b.item, depends_on: b.depends_on, split: b.split, proposal: b.proposal }),
    '/v1/work/tier': (b) => ({ kind: 'tier_changed', item: b.item, tier: b.tier }),
    '/v1/work/note': (b) => ({ kind: 'human_input', item: b.item, input: b.input ?? 'note', text: b.text }),
    '/v1/work/redo': (b) => ({ kind: 'forked_from', item: b.item, from_attempt: b.from_attempt, at: b.at, note: b.note }),
    '/v1/work/workers/stop': (b) => ({ v: 24, kind: 'worker_stopped', worker: b.worker }),
    '/v1/work/close': (b) => ({ kind: 'closed', item: b.item, why: b.why }),
    '/v1/work/questions/answer': (b) => ({ kind: 'answer', item: b.item, question: b.question, text: b.text ?? { text: '' }, chosen: b.chosen, then: { then: 'resume' }, evidence: [] }),
  };
  window.__tracker = { calls: [], refuse: {}, delay: 0, token: 'person-token' };
  const realFetch = window.fetch.bind(window);
  window.fetch = async (url, init) => {
    // Only the tracker's routes are faked; the page's own assets (the WebAssembly) load as usual.
    if (typeof url !== 'string' || !url.startsWith('/v1/')) return realFetch(url, init);
    const t = window.__tracker;
    const body = init.body ? JSON.parse(init.body) : null;
    t.calls.push({ url, auth: init.headers.authorization, body });
    await new Promise((ok) => setTimeout(ok, t.delay));
    const res = (status, json) => ({ status, json: async () => json });
    if (init.headers.authorization !== 'Bearer ' + t.token) return res(401, { error: 'the tracker does not know this token' });
    if (url === '/v1/me') return res(200, { person: 'maintainer', org_admin: true, teams: [{ team: 'default', role: 'admin' }] });
    if (t.refuse[body?.item]) return res(409, t.refuse[body.item]);
    // The real tracker reads an answer's text as content ({ text }) and refuses a bare string.
    if (body && 'text' in body && typeof body.text?.text !== 'string') return res(422, { error: 'text: expected content' });
    if (url.endsWith('/questions/answer') && !body.text === !body.chosen) return res(400, { error: 'answer with one of the options or with text, not both', why: 'invalid' });
    const make = ROUTES[url];
    if (!make) return res(404, { error: 'no such route' });
    window.__push({ actor: 'person', by: 'maintainer', at_ms: Date.now(), ...make(body) });
    return res(200, { kind: make(body).kind, offset: window.__ws.liveAt - 1 });
  };
  window.__push = (record) => window.__ws.reply({ type: 'event', offset: window.__ws.liveAt++, payload: encode(record) });
})();`;

const failures = [];
const check = (ok, what) => {
  console.log(`${ok ? 'ok  ' : 'FAIL'} ${what}`);
  if (!ok) failures.push(what);
};

// CDP modifier bits: Ctrl is 2, Meta is 4. The page reads Cmd as Mod on a Mac.
const MOD = process.platform === 'darwin' ? 4 : 2;
const MOD_NAME = process.platform === 'darwin' ? 'Cmd' : 'Ctrl';
const KEYS = { Escape: 27, Enter: 13, Tab: 9, ArrowDown: 40, ArrowUp: 38, ArrowLeft: 37, ArrowRight: 39, Home: 36, End: 35 };
async function press(page, key, modifiers = 0) {
  const code = KEYS[key];
  // Enter carries text, as on a real keyboard, so a focused link activates.
  const text = key === 'Enter' ? '\r' : code ? undefined : key;
  const base = { key, modifiers, windowsVirtualKeyCode: code ?? key.toUpperCase().charCodeAt(0), code: code ? key : undefined };
  await page.send('Input.dispatchKeyEvent', { type: text && !modifiers ? 'keyDown' : 'rawKeyDown', text: modifiers ? undefined : text, ...base });
  await page.send('Input.dispatchKeyEvent', { type: 'keyUp', ...base });
  await new Promise((ok) => setTimeout(ok, 60));
}

const server = await serve(fileURLToPath(new URL('../dist', import.meta.url)));
const page = await launch();
let script = null;
async function status() {
  return page.eval(`document.querySelector('.status')?.textContent ?? ''`);
}
let loaded = null;
// Any exception or console error on a page is a failure, whatever the flow checks said.
function clean() {
  if (loaded !== null) check(page.problems.length === 0, `${loaded}: no page errors${page.problems.length ? ` (${page.problems.join('; ')})` : ''}`);
}
// Runs before the page's own code: seeds local storage (saved views, density) for one load.
const seed = (entries) => `(() => { try { ${Object.entries(entries).map(([k, v]) => `localStorage.setItem(${JSON.stringify(k)}, ${JSON.stringify(v)});`).join(' ')} } catch {} })();`;
let seedScript = null;
async function open(query, { mode = 'live', theme = 'dark', width = 1280, height = 800, storage = null, log = null, pageSize = 20 } = {}) {
  if (seedScript) await page.send('Page.removeScriptToEvaluateOnNewDocument', { identifier: seedScript });
  seedScript = storage ? (await page.send('Page.addScriptToEvaluateOnNewDocument', { source: seed(storage) })).identifier : null;
  clean();
  loaded = `page ${mode} ${theme} ${width}px`;
  if (script) await page.send('Page.removeScriptToEvaluateOnNewDocument', { identifier: script });
  script = (await page.send('Page.addScriptToEvaluateOnNewDocument', { source: fakeGateway(log ?? fixtureLog(), mode, pageSize) })).identifier;
  await page.setViewport(width, height, 1);
  // The tab keeps its session across navigations (session.ts), so a page opened without a
  // token starts from an empty session, as a new tab would.
  if (!query.includes('token=')) await page.eval(`(() => { try { sessionStorage.clear(); } catch {} return true; })()`);
  await page.goto(`${server.origin}/index.html?theme=${theme}&${query}`);
  await page.eval(`document.fonts.ready.then(() => true)`);
}
const connected = 'room=kbl&token=t&gateway=ws://127.0.0.1:9/ws';
const live = `document.querySelector('.status')?.textContent.includes('Live')`;
const focused = (sel) => page.eval(`document.activeElement?.matches(${JSON.stringify(sel)}) ?? false`);
async function shoot(name) {
  if (!shots) return;
  await new Promise((ok) => setTimeout(ok, 200));
  const shot = await page.send('Page.captureScreenshot', { format: 'png' });
  writeFileSync(join(shots, `${name}.png`), Buffer.from(shot.data, 'base64'));
}
async function axe(name) {
  await page.eval(axeSource);
  const result = await page.eval(`axe.run(document, { runOnly: ['wcag2a', 'wcag2aa', 'wcag21aa', 'wcag22aa', 'best-practice'] })
    .then((r) => r.violations.map((v) => ({ id: v.id, impact: v.impact, nodes: v.nodes.length, help: v.help, targets: v.nodes.slice(0, 3).map((n) => n.target.join(' ')), why: v.nodes.slice(0, 3).map((n) => n.failureSummary) })))`);
  const bad = result.filter((v) => v.impact === 'serious' || v.impact === 'critical');
  for (const v of result) console.log(`     axe ${name}: ${v.impact} ${v.id} (${v.nodes}) ${v.help} [${v.targets.join(' | ')}]`);
  for (const v of result) for (const [i, why] of v.why.entries()) console.log(`       ${v.id} ${v.targets[i]}: ${String(why).replace(/\s+/g, ' ')}`);
  check(bad.length === 0, `axe ${name}: no serious or critical violations (${result.length} total)`);
}

try {
  for (const theme of ['dark', 'light']) {
    await open(connected, { theme });
    await page.waitFor(live);
    const line = await status();
    check(/^. ?Live, \d+ items( · [\w.:\[\]-]+)?$/.test(line), `${theme}: the fixture folds with nothing refused (${line})`);
    const groups = await page.eval(`[...document.querySelectorAll('[id^=group-]')].map((g) => g.id.slice(6)).join(' ')`);
    check(['needs_you', 'held', 'in_review', 'ready', 'triage'].every((s) => groups.includes(s)), `${theme}: the fixture fills the stages (${groups})`);
    await axe(`list ${theme}`);
    await shoot(`list-${theme}`);

    await press(page, 'k', MOD);
    check(await focused('.palette-input'), `${theme}: ${MOD_NAME}+K opens the palette with focus in its input`);
    await axe(`palette ${theme}`);
    await shoot(`palette-${theme}`);
    await press(page, 'Escape');
    check(!(await page.eval(`document.querySelector('dialog.palette').open`)), `${theme}: Esc closes the palette`);

    await press(page, '?');
    check(await page.eval(`document.querySelector('dialog.help').open`), `${theme}: ? opens the shortcuts`);
    await axe(`shortcuts ${theme}`);
    await shoot(`shortcuts-${theme}`);
    await press(page, 'Escape');

    // Spend: the fold's recorded costs per root, tier and repo, said to be a lower bound.
    await page.eval(`[...document.querySelectorAll('.sidebar-foot button')].find((b) => b.textContent === 'Spend').click()`);
    check(await page.eval(`document.querySelector('dialog.spend').open`), `${theme}: the Spend button opens the spend dialog`);
    check(await page.eval(`document.querySelector('.spend-total').textContent.startsWith('$')`), `${theme}: spend shows a total`);
    check(await page.eval(`/lower bound/.test(document.querySelector('.spend-note').textContent)`), `${theme}: spend says it is a lower bound`);
    check(await page.eval(`document.querySelectorAll('#spend-roots ~ table tbody tr').length > 0 && document.querySelectorAll('#spend-repos ~ table tbody tr').length > 0`), `${theme}: spend lists roots and repos`);
    check(await page.eval(`document.querySelector('#spend-tiers ~ table tbody tr[data-key=fast] td')?.textContent === '$0.15'`), `${theme}: spend lists the reviewer tier from the fixture's review`);
    check(await page.eval(`document.querySelector('#spend-models ~ table tbody tr[data-key="claude-opus-5-5"]')?.textContent === 'claude-opus-5-5 check pricingtokens20,000 input, 4,000 output$0.60'`), `${theme}: spend lists a priced model with its usage, cost and the stale-price hint (the fixture's prices date from 2023)`);
    check(await page.eval(`(() => { const r = document.querySelector('#spend-models ~ table tbody tr[data-key="gpt-6"]'); return r?.textContent === 'gpt-6AIC1.5 creditsunpriced'; })()`), `${theme}: spend shows an unpriced model as unpriced with its usage, not $0`);
    check(await focused('dialog.spend button'), `${theme}: the spend dialog takes focus`);
    await axe(`spend ${theme}`);
    await shoot(`spend-${theme}`);
    await page.eval(`document.querySelector('dialog.spend .dialog-body').scrollTop = 1e6`);
    await shoot(`spend-models-${theme}`);
    // Models and prices: reached from Spend, lists every model seen in usage with its unit rows and history.
    await page.eval(`[...document.querySelectorAll('dialog.spend button')].find((b) => b.textContent === 'Models and prices').click()`);
    check(await page.eval(`document.querySelector('dialog.prices').open && !document.querySelector('dialog.spend').open`), `${theme}: Models and prices opens from Spend`);
    check(await page.eval(`!!document.querySelector('dialog.prices section[data-model="claude-opus-5-5"] tr[data-unit=input]') && !!document.querySelector('dialog.prices section[data-model="gpt-6"]')`), `${theme}: the page lists priced and unpriced models seen in usage`);
    check(await page.eval(`document.querySelector('dialog.prices section[data-model="claude-opus-5-5"] tr[data-unit=input]').textContent.includes('person')`), `${theme}: a price shows its source`);
    check(await page.eval(`document.querySelector('dialog.prices section[data-model="claude-opus-5-5"] details summary').textContent.startsWith('History')`), `${theme}: each model has its price history`);
    await axe(`prices ${theme}`);
    await shoot(`prices-${theme}`);
    await press(page, 'Escape');
    check(!(await page.eval(`document.querySelector('dialog.prices').open`)), `${theme}: Esc closes Models and prices`);

    // Metrics: the trust numbers come from the tracker, so with no sign-in the card asks for one.
    await page.eval(`[...document.querySelectorAll('.sidebar-foot button')].find((b) => b.textContent === 'Metrics').click()`);
    check(await page.eval(`document.querySelector('dialog.metrics').open && document.querySelector('#metrics-trust').textContent === 'Trust in summaries'`), `${theme}: the Metrics button opens the trust card`);
    check(await page.eval(`/Sign in/.test(document.querySelector('.metrics-card').textContent)`), `${theme}: signed out, metrics asks to sign in`);
    const flowText = await page.eval(`document.querySelector('.metrics-flow').innerText.replace(/\\s+/g, ' ').trim()`);
    console.log(`${theme}: metrics flow section reads: ${flowText.slice(0, 400)}`);
    check(/Time in each stage|still loading/.test(flowText), `${theme}: metrics shows its flow numbers or says the log is loading`);
    await axe(`metrics ${theme}`);
    await press(page, 'Escape');
  }

  await open(connected);
  await page.waitFor(live);
  await press(page, 'j');
  await press(page, 'j');
  const second = await page.eval(`document.activeElement?.dataset.id ?? null`);
  check(second === (await page.eval(`document.querySelectorAll('.row')[1].dataset.id`)), 'j moves selection and focus down the list');
  await press(page, 'k', MOD);
  await press(page, 'Escape');
  check(await page.eval(`document.activeElement?.dataset.id === ${JSON.stringify(second)}`), 'closing the palette gives focus back to the row');

  const rowCount = await page.eval(`document.querySelectorAll('.row').length`);
  await page.eval(`window.__push({ kind: 'imported', item: 'gh-2230', source: 'github', title: 'A record that lands while you read', upstream_id: 'gh-2230', actor: 'person', by: 'maintainer', at_ms: 1800000000000 })`);
  await page.waitFor(`document.querySelectorAll('.row').length === ${rowCount + 1}`);
  check(await page.eval(`document.activeElement?.dataset.id === ${JSON.stringify(second)}`), 'a live record does not move the focus off the row');

  await press(page, 'Tab');
  const third = await page.eval(`document.activeElement?.dataset.id ?? null`);
  check(third !== second && (await page.eval(`document.activeElement?.hasAttribute('data-selected') ?? false`)), 'Tab to a row selects it');
  await press(page, 'Enter');
  await page.waitFor(`location.search.includes('item=${third}')`);
  check(await page.eval(`!document.querySelector('.panel').hidden && document.querySelector('.panel .row-id').textContent === ${JSON.stringify(third)}`), 'Enter opens the focused row in the panel and the address');
  check(await focused('.panel'), 'opening an item moves focus into the panel');
  await shoot('panel-dark');
  await press(page, 'Escape');
  check(await page.eval(`document.querySelector('.panel').hidden`), 'Esc closes the panel');
  check(await page.eval(`document.activeElement?.dataset.id === ${JSON.stringify(third)}`), 'closing the panel gives focus back to the row');

  await press(page, 'k', MOD);
  for (const ch of 'shortcuts') await press(page, ch);
  await press(page, 'Enter');
  check(await page.eval(`document.querySelector('dialog.help').open`), 'the palette runs a command by name');
  await press(page, 'Escape');

  await press(page, 'k', MOD);
  for (const ch of 'search') await press(page, ch);
  await press(page, 'Enter');
  check(await focused('#search'), 'Search items from the palette leaves focus in the search field');
  await press(page, 'Escape');

  await press(page, '/');
  check(await focused('#search'), '/ focuses search');
  for (const ch of 'zzqx') await press(page, ch);
  check(await page.eval(`document.querySelector('.empty h2')?.textContent.startsWith('Nothing matches')`), 'a search that matches nothing says so');
  await axe('no matches');
  await shoot('empty-search-dark');
  await press(page, 'Escape');
  check(await page.eval(`document.querySelectorAll('.row').length > 0`), 'Esc clears the search');

  // Board: V switches, j/k move in a column, l/h across, Enter opens; one Tab stop per column.
  await open(connected);
  await page.waitFor(live);
  await press(page, 'j');
  await press(page, 'v');
  await page.waitFor(`!!document.querySelector('.board .column')`);
  check((await page.eval(`document.querySelector('.segment[data-layout=board]').getAttribute('aria-pressed') === 'true'`)), 'V switches to the board and the layout button says so');
  check(await focused('.card[data-selected]'), 'switching layout keeps focus on the selected item');
  const columns = await page.eval(`[...document.querySelectorAll('.column')].map((c) => c.querySelectorAll('.card').length)`);
  check(await page.eval(`[...document.querySelectorAll('.column')].every((c) => c.querySelectorAll('.card[tabindex="0"]').length === 1)`), `each column is one Tab stop (${columns.join(' ')})`);
  await page.waitFor(`location.search.includes('layout=board')`);
  check(true, 'the board is in the address');
  const colOf = `[...document.querySelectorAll('.column')].findIndex((c) => c.contains(document.activeElement))`;
  await press(page, 'l');
  check((await page.eval(colOf)) === 1, 'l moves to the next column');
  await press(page, 'ArrowRight');
  await press(page, 'h');
  check((await page.eval(colOf)) === 1, 'h and the side arrows move back and forth across columns');
  await press(page, 'Tab');
  check((await page.eval(colOf)) === 2, 'Tab goes to the next column, not the next card');
  const card = await page.eval(`document.activeElement.dataset.id`);
  await press(page, 'Enter');
  await page.waitFor(`location.search.includes('item=${card}')`);
  check(await focused('.panel'), 'Enter on a card opens the panel');
  check(await page.eval(`[...document.querySelectorAll('.panel dt')].map((d) => d.textContent).join(',').startsWith('Upstream,Stage')`), 'the panel shows the recorded fields');
  await shoot('board-panel-dark');
  await press(page, 'Escape');
  check(await page.eval(`document.activeElement?.dataset.id === ${JSON.stringify(card)}`), 'closing the panel gives focus back to the card');
  await press(page, 'End');
  const lastInColumn = await page.eval(`(() => { const c = document.activeElement.closest('.column'); return document.activeElement === [...c.querySelectorAll('.card')].at(-1); })()`);
  check(lastInColumn && (await page.eval(colOf)) === 2, 'End goes to the last card of the column, not of the board');

  // Swimlanes: the board grouped by tier is a row per tier under the same stage columns.
  await open(`${connected}&layout=board&group=tier`);
  await page.waitFor(`!!document.querySelector('.board.lanes .lane')`);
  const lanes = await page.eval(`[...document.querySelectorAll('.lane')].map((l) => l.querySelector('.group-toggle .group-name').textContent + ':' + l.querySelector('.group-toggle').getAttribute('aria-expanded') + ':' + l.querySelectorAll('.card').length).join(' ')`);
  console.log(`swimlanes saw: ${lanes}`);
  check(await page.eval(`document.querySelectorAll('.lane').length > 1 && !document.querySelector('#group-by').disabled`), `the board's Group by is enabled and makes lanes (${lanes})`);
  check(await page.eval(`[...document.querySelectorAll('.column')].every((c) => c.closest('.lane'))`), 'every column cell sits in a lane');
  check(await page.eval(`new Set([...document.querySelectorAll('.lane')].map((l) => l.querySelectorAll('.column').length)).size === 1`), 'every lane has the same stage columns');
  await page.eval(`document.querySelector('.group-toggle').focus()`);
  await press(page, 'Enter');
  await page.waitFor(`document.querySelector('.group-toggle').getAttribute('aria-expanded') === 'false'`);
  check(await page.eval(`document.activeElement === document.querySelector('.group-toggle') && document.querySelector('.lane').querySelectorAll('.card').length === 0`), 'Enter on a lane header folds it and keeps the focus on the header');
  await page.eval(`document.querySelectorAll('.card')[0].focus()`);
  await press(page, 'k');
  check(await page.eval(`!!document.activeElement.closest('.card') && !!document.activeElement.closest('.lane') && document.activeElement.closest('.lane') !== document.querySelector('.lane')`), 'k never lands on a card of the folded lane');
  await page.eval(`document.querySelector('.group-toggle').click()`);
  await page.waitFor(`document.querySelector('.group-toggle').getAttribute('aria-expanded') === 'true'`);
  check(await page.eval(`location.search.includes('group=tier') && location.search.includes('layout=board')`), 'the board grouping is in the address, so a saved view keeps it');
  await axe('board swimlanes');

  // Live records on the board: a card that leaves the filter hands the focus to its column,
  // a card that moves column keeps it. Both are announced, and the selection follows.
  // The board was once ?view=board: such a link still opens it, and the address moves to ?layout=board.
  await open(`${connected}&view=board&tier=1`);
  await page.waitFor(live);
  await page.waitFor(`location.search.includes('layout=board') && !location.search.includes('view=')`);
  check(await page.eval(`!!document.querySelector('.board .column')`), 'an old ?view=board link opens the board and is rewritten to ?layout=board');
  const announced = () => page.eval(`document.querySelector('.announce')?.textContent ?? ''`);
  const person = `actor: 'person', by: 'maintainer', at_ms: 1800000000000`;
  await page.eval(`window.__push({ kind: 'imported', item: 'gh-3001', source: 'github', title: 'A second ready card', upstream_id: 'GetFelix/felix#3001', ${person} })`);
  await page.eval(`window.__push({ kind: 'accepted', item: 'gh-3001', tier: 1, ${person} })`);
  await page.waitFor(`!!document.querySelector('.column:has(.card[data-id=gh-2219]) .card[data-id=gh-3001]')`);
  await page.eval(`document.querySelector('.card[data-id=gh-3001]').focus(); true`);
  await page.eval(`window.__push({ kind: 'tier_changed', item: 'gh-3001', tier: 2, ${person} })`);
  await page.waitFor(`!document.querySelector('.card[data-id=gh-3001]')`);
  await page.waitFor(`document.querySelector('.card[data-selected]')?.dataset.id === 'gh-2219'`);
  check(await focused('.card[data-id=gh-2219]'), 'a focused card re-triaged out of the filter hands the focus to its column, and the selection follows');
  check((await announced()) === 'felix#3001 is no longer in this view.', `the card leaving is announced (${await announced()})`);
  await page.eval(`window.__push({ kind: 'claimed', item: 'gh-2219', attempt: 1, deadline_ms: 1900000000000, actor: 'agent', by: 'agent-1', at_ms: 1800000000001 })`);
  await page.waitFor(`!!document.querySelector('#column-held + ul .card[data-id=gh-2219]')`);
  check(await focused('.card[data-id=gh-2219]'), 'a card a record moves to another column keeps the focus');
  check((await announced()) === 'felix#2219 moved to Working.', `the move is announced (${await announced()})`);

  // Child work nests under its root, cards lead with the upstream reference, and the Working column says who holds what.
  await open(`${connected}&layout=board`);
  await page.waitFor(live);
  check(await page.eval(`!document.querySelector('.card[data-id=rev-2300]')`), 'a review child is not a card of its own');
  check(await page.eval(`document.querySelector('.card[data-id=gh-2300] .card-lead').textContent === 'felix#2300'`), 'a card leads with the upstream reference, not the KBL id');
  check(await page.eval(`document.querySelector('[data-chip=gh-2300]')?.textContent.includes('1 sub-task, 1 done')`), 'the root carries a sub-task count and progress chip');
  check(await page.eval(`document.querySelector('[data-chip=gh-2300]').getAttribute('aria-expanded') === 'false'`), 'the chip starts closed');
  // A script click does not focus a button the way a real one does, so focus first.
  await page.eval(`document.querySelector('[data-chip=gh-2300]').focus(); document.querySelector('[data-chip=gh-2300]').click(); true`);
  await page.waitFor(`document.querySelector('[data-chip=gh-2300]')?.getAttribute('aria-expanded') === 'true'`);
  check(await page.eval(`document.querySelector('#children-gh-2300 .child-link').textContent.includes('Review the cache fix')`), 'the chip opens the children as links');
  check(await focused('[data-chip=gh-2300]'), 'the chip keeps the focus when it opens');
  // Opening a child from its root's chip must leave the selection on a card that is drawn.
  await page.eval(`document.querySelector('#children-gh-2300 .child-link').click(); true`);
  await page.waitFor(`!document.querySelector('.panel').hidden`);
  const picked = await page.eval(`document.querySelector('[data-selected]')?.dataset.id ?? null`);
  console.log(`  after opening rev-2300 from its chip the selection is ${picked}`);
  check(picked === 'gh-2300', 'opening a child from its root chip selects the visible root');
  await press(page, 'Escape');
  await page.waitFor(`document.querySelector('.panel').hidden`);
  check(await page.eval(`document.activeElement?.closest('[data-id]')?.dataset.id === 'gh-2300'`), 'closing the child returns the focus to the root card');
  await page.eval(`document.querySelector('[data-chip=gh-2300]').focus(); true`);
  await axe('board nested');
  await shoot('board-nested-dark');
  check(await page.eval(`document.querySelector('#column-held').textContent.startsWith('Working')`), 'the held column is called Working');
  const working = await page.eval(`document.querySelector('#column-held + ul .card-working')?.textContent ?? ''`);
  check(/ working, attempt 1/.test(working), `a working card names the agent, attempt and last activity (${working})`);
  await shoot('board-working-dark');
  await page.eval(`[...document.querySelectorAll('.filter-bar button')].find((b) => b.textContent.startsWith('Child cards')).click(); true`);
  await page.waitFor(`!!document.querySelector('.card[data-id=rev-2300]')`);
  check(await page.eval(`!document.querySelector('.child-strip') && [...document.querySelectorAll('.filter-bar button')].find((b) => b.textContent.startsWith('Child cards')).getAttribute('aria-pressed') === 'true'`), 'the toggle shows child work as cards and drops the strips');
  await axe('board child cards');
  await shoot('board-child-cards-dark');
  // The choice is remembered per viewer, so turn it off or the list below shows child cards too.
  await page.eval(`[...document.querySelectorAll('.filter-bar button')].find((b) => b.textContent.startsWith('Child cards')).click(); true`);
  await page.waitFor(`!document.querySelector('.card[data-id=rev-2300]')`);
  await open(connected);
  await page.waitFor(live);
  check(await page.eval(`!document.querySelector('.row[data-id=rev-2300]') && !!document.querySelector('.row[data-id=gh-2300] ~ .child-strip, li:has(.row[data-id=gh-2300]) .child-strip')`), 'the list nests children under their root too');

  // A filter that takes the focused row away hands the focus to the next row still shown.
  await open(connected);
  await page.waitFor(live);
  await page.eval(`document.querySelector('.row[data-id=gh-2204]').focus(); true`);
  await press(page, 'k', MOD);
  for (const ch of 'tier: 2') await press(page, ch);
  await press(page, 'Enter');
  await page.waitFor(`!document.querySelector('.row[data-id=gh-2204]')`);
  await page.waitFor(`document.querySelector('.row[data-selected]')?.dataset.id === 'gh-2210'`);
  check(await focused('.row[data-id=gh-2210]'), 'a filter that hides the focused row moves the focus to the next row, not the body');
  for (const theme of ['dark', 'light']) for (const density of ['comfortable', 'compact']) {
    await open(`${connected}&layout=board`, { theme, storage: { 'kbl.dashboard.density': density } });
    await page.waitFor(live);
    check((await page.eval(`document.documentElement.dataset.density`)) === density, `${theme} ${density}: density is remembered`);
    await axe(`board ${theme} ${density}`);
    await shoot(`board-${theme}-${density}`);
  }
  await open(connected, { storage: { 'kbl.dashboard.density': 'compact' } });
  await page.waitFor(live);
  await axe('list compact');
  await shoot('list-dark-compact');

  // Filters: a select per recorded facet, a summary, Clear filters, and palette commands.
  await open(connected);
  await page.waitFor(live);
  const offered = await page.eval(`[...document.querySelectorAll('.filters .filter select')].map((s) => s.dataset.facet).join(' ')`);
  check(offered === 'source tier repo', `filters offer the recorded facets (${offered})`);
  const shownValues = await page.eval(`[...document.querySelectorAll('.filters .filter select')].map((s) => s.selectedOptions[0]?.textContent ?? '').join(',')`);
  check(shownValues === 'Any,Any,Any', `every filter with no value shows Any, not a blank (${shownValues})`);
  await press(page, 'f');
  check(await focused('#filter-source'), 'F focuses the first filter');
  await page.eval(`(() => { const s = document.querySelector('#filter-repo'); s.value = 'GetFelix/felix'; s.dispatchEvent(new Event('change')); return true; })()`);
  await page.waitFor(`location.search.includes('repo=GetFelix')`);
  const repoRows = await page.eval(`document.querySelectorAll('.row').length`);
  check(repoRows === 6, `a repo filter keeps only that repo's items (${repoRows})`);
  check(await page.eval(`document.querySelector('.filter-summary').textContent === 'Showing 6 of 18: Repo GetFelix/felix'`), 'the summary says what the filters hide');
  await press(page, 'k', MOD);
  for (const ch of 'tier: 2') await press(page, ch);
  await press(page, 'Enter');
  await page.waitFor(`document.querySelectorAll('.row').length === 1`);
  check(await page.eval(`document.querySelector('.row').dataset.id === 'gh-2210'`), 'a palette command adds a filter, and filters combine');
  await axe('filtered');
  await shoot('filters-dark');
  // Focus is back on the filter, which keeps its own typeahead keys; go to search directly.
  await page.eval(`document.querySelector('#search').focus(); true`);
  for (const ch of 'zzqx') await press(page, ch);
  check(await page.eval(`[...document.querySelectorAll('.empty button')].map((b) => b.textContent).join(',') === 'Clear search,Clear filters'`), 'no match inside filters offers Clear search and Clear filters');
  await press(page, 'Escape');
  await page.eval(`[...document.querySelectorAll('.filter-bar button')].find((b) => b.textContent === 'Clear filters').click(); true`);
  await page.waitFor(`document.querySelectorAll('.row').length === 17 && !location.search.includes('tier=')`);
  check(true, 'Clear filters shows everything and clears the address');

  // Saved views: from the address, to the sidebar, rename, delete, and back from storage.
  await open(`${connected}&layout=board&source=github&tier=1`);
  await page.waitFor(live);
  check((await page.eval(`document.querySelectorAll('.card').length`)) === 6, 'a link with a view and filters opens that view');
  await press(page, 'k', MOD);
  for (const ch of 'save this') await press(page, ch);
  await press(page, 'Enter');
  check(await focused('#view-name'), 'Save this view asks for a name');
  check((await page.eval(`document.querySelector('#view-name').value`)) === 'Source github, Tier 1, board', 'the name starts from the filters');
  await axe('save view');
  await shoot('save-view-dark');
  await press(page, 'Enter');
  await page.waitFor(`!!document.querySelector('.saved-row a')`);
  check(await page.eval(`document.querySelector('.saved-row a').getAttribute('aria-current') === 'page' && document.querySelector('.topbar h1').textContent === 'Source github, Tier 1, board'`), 'the saved view is in the sidebar, current, and names the page');
  const saved = await page.eval(`localStorage.getItem('kbl.dashboard.views')`);
  check(saved === '[{"name":"Source github, Tier 1, board","search":"layout=board&source=github&tier=1"}]', 'the view is stored for this viewer');
  await shoot('saved-view-dark');
  await page.eval(`document.querySelector('.saved-row button').click(); true`);
  await page.eval(`(() => { const i = document.querySelector('#view-name'); i.value = 'Urgent GitHub'; return true; })()`);
  await press(page, 'Enter');
  await page.waitFor(`document.querySelector('.saved-row a')?.textContent === 'Urgent GitHub'`);
  check(await focused('.saved-row button[aria-label="Edit view Urgent GitHub"]'), 'Edit renames a view and the focus is back on its Edit button');
  await page.eval(`document.querySelector('.nav a').click(); true`);
  await page.waitFor(`document.querySelectorAll('.row').length === 17`);
  check(await page.eval(`document.querySelector('.nav a').getAttribute('aria-current') === 'page' && !document.querySelector('.saved-row a').hasAttribute('aria-current')`), 'All work resets the view');
  await page.eval(`document.querySelector('.saved-row a').click(); true`);
  await page.waitFor(`document.querySelectorAll('.card').length === 6`);
  check(true, 'a saved view opens from the sidebar');
  await page.eval(`document.querySelector('.saved-row button').click(); true`);
  await page.eval(`[...document.querySelectorAll('.view-dialog button')].find((b) => b.textContent === 'Delete view').click(); true`);
  await page.waitFor(`!document.querySelector('.saved-row a')`);
  check((await page.eval(`localStorage.getItem('kbl.dashboard.views')`)) === '[]', 'Delete removes the view from the sidebar and storage');
  check(await page.eval(`document.activeElement?.textContent === 'Save view'`), 'deleting the last view leaves the focus on Save view');
  await open(connected, { storage: { 'kbl.dashboard.views': '[{"name":"Tier 1","search":"tier=1"},{"name":"Tier 2","search":"tier=2"}]' } });
  await page.waitFor(live);
  // Another tab adds a view: this one shows it, and its own next write keeps it.
  await page.eval(`(() => { const v = '[{"name":"Tier 1","search":"tier=1"},{"name":"Tier 2","search":"tier=2"},{"name":"Other tab","search":"source=jira"}]'; localStorage.setItem('kbl.dashboard.views', v); dispatchEvent(new StorageEvent('storage', { key: 'kbl.dashboard.views', newValue: v })); return true; })()`);
  await page.waitFor(`document.querySelectorAll('.saved-row').length === 3`);
  check(true, 'a view saved in another tab shows up');
  await page.eval(`document.querySelector('.saved-row button').click(); true`);
  await page.eval(`[...document.querySelectorAll('.view-dialog button')].find((b) => b.textContent === 'Delete view').click(); true`);
  await page.waitFor(`document.querySelectorAll('.saved-row').length === 2`);
  check(await focused('.saved-row a') && (await page.eval(`document.activeElement.textContent`)) === 'Tier 2', 'deleting a view moves the focus to the next saved view');
  check(JSON.parse(await page.eval(`localStorage.getItem('kbl.dashboard.views')`)).map((v) => v.name).join(',') === 'Tier 2,Other tab', 'a write keeps the view the other tab saved');
  await open(connected, { storage: { 'kbl.dashboard.views': '[{"name":"Tier 2","search":"tier=2"}]' } });
  await page.waitFor(live);
  check((await page.eval(`document.querySelector('.saved-row a')?.textContent`)) === 'Tier 2', 'saved views come back on the next visit');
  await open(connected, { storage: { 'kbl.dashboard.views': '[{"name":"Old board","search":"view=board&tier=1"}]' } });
  await page.waitFor(live);
  await page.eval(`document.querySelector('.saved-row a').click(); true`);
  await page.waitFor(`location.search.includes('layout=board')`);
  check(await page.eval(`!!document.querySelector('.board .column') && document.querySelector('.saved-row a').getAttribute('aria-current') === 'page'`), 'a stored legacy view opens the board and is current in the sidebar');
  await open(connected, { storage: { 'kbl.dashboard.views': 'not json' } });
  await page.waitFor(live);
  check(await page.eval(`!!document.querySelector('.nav-empty')`), 'unreadable storage leaves the sidebar empty, not broken');
  // The Blocked view: reached from the palette, left from the sidebar.
  const rowIds = `[...document.querySelectorAll('.row')].map((r) => r.dataset.id).join(' ')`;
  const rowText = (id) => page.eval(`document.querySelector('.row[data-id=${id}] .row-waits')?.textContent ?? ''`);
  await press(page, 'k', MOD);
  for (const ch of 'blocked') await press(page, ch);
  await press(page, 'Enter');
  await page.waitFor(`location.search.includes('view=blocked')`);
  check((await page.eval(rowIds)) === 'blk-40 blk-41', `the palette opens the Blocked view with only blocked items (${await page.eval(rowIds)})`);
  check(await page.eval(`document.querySelector('h1').textContent === 'Blocked' && document.querySelector('.nav a[aria-current=page]').dataset.view === 'blocked'`), 'the Blocked view is the current sidebar entry and the heading');
  const waits40 = await rowText('blk-40');
  check(waits40 === 'waits on gw-31 (github:GetFelix/felix-gateway), jira-300 (jira)', `a blocked row names blockers in another repo and source (${waits40})`);
  const waits41 = await rowText('blk-41');
  check(waits41 === 'waits on blk-42 release', `a blocker in the same repo awaiting its release says release only (${waits41})`);
  await axe('blocked dark');
  await shoot('blocked-dark');
  await press(page, 'j');
  await press(page, 'Enter');
  await page.waitFor(`location.search.includes('item=blk-40')`);
  const blockers = await page.eval(`[...document.querySelectorAll('.panel .blockers li')].map((l) => l.textContent).join(' | ')`);
  check(
    blockers === 'gw-31 Compare-and-set on the gateway (github:GetFelix/felix-gateway) | jira-300 Agree the write semantics (jira)',
    `the panel lists each blocker with its title and where it lives (${blockers})`,
  );
  await axe('blocked panel');
  await shoot('blocked-panel-dark');
  await page.eval(`document.querySelector('.panel .blockers a[data-item=gw-31]').focus()`);
  await page.eval(`window.__push({ kind: 'linked', item: 'blk-40', to: 'gh-2204', actor: 'person', by: 'maintainer', at_ms: 1800000000040 })`);
  await page.waitFor(`document.querySelector('.panel .blockers')?.textContent.includes('gh-2204')`);
  check(await page.eval(`document.activeElement?.dataset.focus === 'blocker:gw-31'`), 'a live redraw of the panel leaves focus on the same blocker link');
  await page.eval(`document.querySelector('.panel .blockers a[data-item=gw-31]').click()`);
  await page.waitFor(`location.search.includes('item=gw-31')`);
  check(await page.eval(`document.querySelector('.panel .row-id').textContent === 'gw-31'`), 'a blocker link opens that blocker in the panel');
  await press(page, 'Escape');
  await page.eval(`document.querySelector('.nav a[data-view=all]').click()`);
  await page.waitFor(`!location.search.includes('view=')`);
  check((await page.eval(`document.querySelectorAll('.row').length`)) > 2, 'the sidebar goes back to all work');

  // Blocked composes with filters and the board; the palette keeps the filters, and cards say what they wait on.
  await open(`${connected}&tier=1&layout=board`);
  await page.waitFor(live);
  await press(page, 'k', MOD);
  for (const ch of 'show blocked') await press(page, ch);
  await press(page, 'Enter');
  await page.waitFor(`location.search.includes('view=blocked')`);
  check(await page.eval(`location.search.includes('tier=1') && location.search.includes('layout=board')`), 'Show blocked work keeps the filters and the layout');
  const cardIds = await page.eval(`[...document.querySelectorAll('.card')].map((c) => c.dataset.id).sort().join(' ')`);
  check(cardIds === 'blk-40 blk-41', `the Blocked board shows only blocked items inside the filter (${cardIds})`);
  const cardWaits = await page.eval(`document.querySelector('.card[data-id=blk-40] .card-waits')?.textContent ?? ''`);
  check(cardWaits === 'waits on gw-31 (github:GetFelix/felix-gateway), jira-300 (jira)', `a blocked card says what it waits on (${cardWaits})`);
  check(await page.eval(`document.querySelector('#filter-tier')?.selectedOptions[0]?.textContent.startsWith('1') === true`), 'the active filter shows its value');
  await axe('blocked board');
  await shoot('blocked-board-dark');

  // The same on the Blocked board: the focused card leaves, the focus goes to a card in its column, then to main.
  const ids = (n) => `actor: 'person', by: 'maintainer', at_ms: 18000000000${n}`;
  await open(`${connected}&view=blocked&layout=board`);
  await page.waitFor(live);
  await page.eval(`window.__push({ kind: 'imported', item: 'blk-43', source: 'github', title: 'Another card waiting on the history fix', upstream_id: 'GetFelix/felix-blocks#43', ${ids(10)} })`);
  await page.eval(`window.__push({ kind: 'accepted', item: 'blk-43', tier: 1, ${ids(11)} })`);
  await page.eval(`window.__push({ kind: 'linked', item: 'blk-43', to: 'gh-2204', ${ids(12)} })`);
  await page.waitFor(`!!document.querySelector('.card[data-id=blk-43]')`);
  const column = (id) => page.eval(`document.querySelector('.card[data-id=${id}]')?.closest('.column')?.getAttribute('aria-labelledby') ?? ''`);
  check((await column('blk-40')) === (await column('blk-41')) && (await column('blk-41')) === (await column('blk-43')), 'the blocked cards share a column');
  await page.eval(`document.querySelector('.card[data-id=blk-40]').focus()`);
  for (const item of ['gw-31', 'jira-300']) {
    await page.eval(`window.__push({ kind: 'closed', item: '${item}', why: 'merged', ${ids(20)} })`);
  }
  await page.waitFor(`!document.querySelector('.card[data-id=blk-40]')`);
  check(await focused('.card[data-id=blk-41]'), `an unblocked card hands the focus to the next card in its column (${await page.eval('document.activeElement?.dataset.id')})`);
  await page.eval(`window.__push({ kind: 'closed', item: 'gh-2204', why: 'merged', ${ids(30)} })`);
  await page.eval(`window.__push({ kind: 'shipped', item: 'blk-42', release: 'v1', ${ids(31)} })`);
  await page.waitFor(`!document.querySelector('.card')`);
  check((await focused('main')) && (await page.eval(`!!document.querySelector('.empty')`)), 'an emptied Blocked board leaves the focus on main with the empty state');

  // A live record that unblocks the focused row hands the focus to a neighbour, then to main.
  await open(`${connected}&view=blocked`);
  await page.waitFor(live);
  const blockedCount = () => page.eval(`document.querySelector('.nav a[data-view=blocked] .count')?.textContent ?? ''`);
  const onRowOrMain = () => page.eval(`document.activeElement?.matches('.row, main') ?? false`);
  check((await blockedCount()) === '2', 'the sidebar counts the blocked items');
  await page.eval(`document.querySelector('.row[data-id=blk-40]').focus()`);
  for (const item of ['gw-31', 'jira-300']) {
    await page.eval(`window.__push({ kind: 'closed', item: '${item}', why: 'merged', actor: 'person', by: 'maintainer', at_ms: 1800000000001 })`);
  }
  await page.waitFor(`!document.querySelector('.row[data-id=blk-40]')`);
  check((await page.eval(rowIds)) === 'blk-41' && (await blockedCount()) === '1', 'closing both blockers drops the row and the sidebar count');
  check(await focused('.row[data-id=blk-41]'), 'the focus moves to the neighbouring row');
  await page.eval(`window.__push({ kind: 'shipped', item: 'blk-42', release: 'v1', actor: 'person', by: 'maintainer', at_ms: 1800000000002 })`);
  await page.waitFor(`!document.querySelector('.row')`);
  check((await onRowOrMain()) && (await focused('main')), 'an emptied view leaves the focus on main, not the body');

  for (const theme of ['dark', 'light']) {
    await open(`${connected}&view=blocked&item=blk-41`, { theme });
    await page.waitFor(live);
    check((await page.eval(rowIds)) === 'blk-40 blk-41' && !(await page.eval(`document.querySelector('.panel').hidden`)), `${theme}: a link opens the Blocked view with the item`);
    await axe(`blocked link ${theme}`);
  }

  await open(`${connected}&item=gh-2219`);
  await page.waitFor(live);
  const flag = (id) => page.eval(`document.querySelector('.row[data-id=${id}] .row-flag:not([hidden])')?.textContent ?? ''`);
  check((await flag('gh-2219')) === '1 file overlap' && (await flag('gh-2215')) === '1 file overlap', 'both rows of an overlap carry a flag');
  check((await flag('gh-2204')) === '', 'a row without overlaps has no flag');
  const note = await page.eval(`document.querySelector('.panel .overlap')?.textContent ?? ''`);
  check(note.includes('gh-2215') && note.includes('core/src/segment/reader.rs') && note.includes('Suggested: gh-2219 waits on gh-2215.'), `the panel names the other item, the shared file and the suggested link (${note})`);
  await axe('overlap panel');
  await shoot('overlap-dark');
  check(await page.eval(`document.querySelector('.panel .overlap a')?.getAttribute('href')?.includes('item=gh-2215') === true`), 'the other item in a note is a link to it');
  await page.eval(`document.querySelector('.panel .overlap a').focus()`);
  await page.eval(`window.__push({ kind: 'context', item: 'gh-2204', entry: 'files', text: { text: 'core/src/segment/reader.rs' }, actor: 'person', by: 'maintainer', at_ms: 1800000000001 })`);
  await page.waitFor(`document.querySelector('.panel .overlap-notes')?.children.length === 2`);
  check((await flag('gh-2219')) === '2 file overlaps' && (await flag('gh-2204')) === '2 file overlaps', 'a live files entry adds a note and updates the flags');
  check(await page.eval(`document.activeElement?.dataset.focus === 'other:gh-2215'`), 'a rebuild of the panel leaves focus on the same link');
  await open(`${connected}&layout=board`);
  await page.waitFor(live);
  const cardFlag = (id) => page.eval(`document.querySelector('.card[data-id=${id}] .row-flag:not([hidden])')?.textContent ?? ''`);
  check((await cardFlag('gh-2219')) === '1 file overlap' && (await cardFlag('gh-2204')) === '', 'board cards carry the same overlap flag');

  // The story: a cited step is a record the reference lands on, and recorded text (a `"` or `]` in a
  // command) cannot break the panel.
  await open(`${connected}&item=sv-1`);
  await page.waitFor(live);
  const at = () => page.eval('window.__ws.liveAt');
  const rec = (r) => page.eval(`window.__push({ actor: 'agent', by: 'agent-1', at_ms: Date.now(), item: 'sv-1', attempt: 1, ...${JSON.stringify(r)} })`);
  await page.eval(`window.__push({ kind: 'imported', item: 'sv-1', source: 'github', title: 'Story of a fix', upstream_id: 'GetFelix/felix#9001', actor: 'person', by: 'maintainer', at_ms: Date.now() })`);
  await page.eval(`window.__push({ kind: 'accepted', item: 'sv-1', tier: 1, actor: 'person', by: 'maintainer', at_ms: Date.now() })`);
  await rec({ kind: 'claimed', deadline_ms: Date.now() + 3_600_000 });
  const stepAt = await at();
  await rec({ kind: 'step', command: 'grep "a]b" -r core/', exit_code: 1, output: { text: 'nothing' } });
  const decisionAt = await at();
  await rec({ kind: 'decision', phase: 'fix', so_far: 'guarding the read', question: 'how', chosen: 'fill before read', why: 'the tests pass', commit: 'c1', evidence: [{ stream: 'work.events', offset: stepAt }] });
  await page.waitFor(`!!document.querySelector('.panel .story .story-step')`);
  check(await page.eval(`document.querySelector('.panel .story .story-step')?.dataset.at === '${stepAt}'`), 'a step a decision cites is a record in the story');
  await page.eval(`document.querySelector('.story-evidence .story-ref').click()`);
  check(await focused(`.story-step[data-at="${stepAt}"]`), 'a step citation moves the focus to the step');
  await page.eval(`document.querySelector('.panel').focus()`);
  await page.eval(`[...document.querySelectorAll('.story-flag[data-rule=cited_step_failed] .story-ref')].find((b) => b.textContent.startsWith('offset')).click()`);
  check(await focused(`.story-step[data-at="${stepAt}"]`), 'the offset in a failed-step flag moves the focus to the step');
  await page.eval(`document.querySelector('.story-evidence .story-ref').focus()`);
  await rec({ kind: 'edit', file: 'core/src/cache.rs', hunk: { text: '@@ -1 +1 @@' }, decision: decisionAt });
  await page.waitFor(`!!document.querySelector('.story-edit')`);
  check(await page.eval(`document.activeElement?.dataset.focus?.startsWith('ref:') === true && !!document.querySelector('.panel .story')`), 'a rebuild with a quote in a recorded command keeps the panel and the focus');
  await axe('story panel');

  // Item detail: the Now line, the live timeline and the attempts. Times are relative and live, and
  // a fold a person opened stays open when a record redraws the panel.
  const text = (sel) => page.eval(`document.querySelector(${JSON.stringify(sel)})?.textContent ?? ''`);
  check(/^agent-1 is working on this, attempt 1, last activity /.test(await text('.panel .now-text')), `the panel says who works on the item (${await text('.panel .now-text')})`);
  const newest = await text('.panel .tl-entry:first-child .tl-head strong');
  check(newest === 'Edited core/src/cache.rs', `the timeline lists the newest record first (saw "${newest}")`);
  check(await page.eval(`!!document.querySelector('.panel .tl-entry time[datetime]')`), 'each timeline entry has a machine-readable time');
  await shoot('panel-now');
  await page.eval(`document.querySelector('.panel .tl-output summary').click()`);
  await page.eval(`document.querySelector('.panel .tl-output summary').focus()`);
  await rec({ kind: 'progress', note: { text: 'moved the fill before the read' } });
  await page.waitFor(`document.querySelector('.panel .tl-entry:first-child .tl-head strong')?.textContent === 'Progress note'`);
  check(await page.eval(`document.querySelector('.panel .tl-output')?.open === true`), 'a live record keeps an opened output open');
  check(await page.eval(`document.activeElement?.dataset.focus?.startsWith('out:') === true`), 'a live record keeps the focus on the output toggle');
  await shoot('panel-timeline');
  await axe('panel timeline');
  // From format 8 a report carries usage and the fold prices it; a reported cost is refused. A record
  // that needs format 8 must say so with `v`, or the fold refuses it as Format.
  await rec({ v: 8, kind: 'done', usage: [{ unit: 'tokens', model: 'claude-opus-5-5', input: 10_000 }] });
  await page.waitFor(`document.querySelector('.panel .attempt')?.textContent.includes('Finished')`);
  check((await text('.panel .attempt')).includes('cost $0.15'), `an attempt shows what it cost (${await text('.panel .attempt')})`);
  check((await text('.panel .attempt')).includes('moved the fill before the read'), `an attempt shows the summary it handed off (saw "${await text('.panel .attempt')}")`);
  check(/^Finished, in review/.test(await text('.panel .now-text')), `the Now line follows the item once the attempt finishes (saw "${await text('.panel .now-text')}")`);
  await page.eval(`document.querySelector('.panel .attempt [data-focus="replay:1"]').click()`);
  check(await focused('.story-decision[data-attempt="1"]'), 'the replay link moves the focus to the attempt\'s first decision');
  await shoot('panel-history');
  await axe('panel history');

  // A fresh page: after a blur, Chrome starts Tab from where focus was, not the top.
  await open(connected);
  await page.waitFor(live);
  await press(page, 'Tab');
  check(await focused('.skip'), 'the first Tab reaches the skip link');

  // A link that names a gateway but carries no token must not move the saved token to it.
  await open(connected);
  await page.waitFor(live);
  check((await status()).includes('127.0.0.1:9'), `the status names the gateway host (saw "${await status()}")`);
  await page.goto(`${server.origin}/index.html?gateway=wss://evil.example/ws&room=kbl`);
  await page.waitFor(live);
  const urls = await page.eval(`JSON.stringify(window.__urls)`);
  console.log(`gateway-only link opened sockets: ${urls}`);
  check(!urls.includes('evil.example'), 'a link with a gateway and no token keeps the saved gateway');
  check(urls.includes('127.0.0.1:9'), 'the saved gateway is the one used');
  await open('room=kbl&token=t&gateway=ws://evil.example/ws');
  await page.waitFor(`document.querySelector('.status')?.textContent.includes('not allowed')`);
  check(await page.eval(`!(window.__urls ?? []).length`), 'a gateway address that is not allowed opens no socket');

  await open('');
  check(await page.eval(`document.querySelector('.empty h2')?.textContent === 'Connect to a gateway'`), 'without a token the page says how to connect');
  await axe('setup');
  await shoot('setup-dark');

  await open(connected, { mode: 'loading' });
  check(await page.eval(`document.querySelector('.list').hasAttribute('aria-busy')`), 'the first read shows placeholder rows');
  await page.waitFor(`document.querySelector('.status')?.textContent.includes('Reading history')`);
  check(true, 'the status says it is reading history');
  await shoot('loading-dark');
  await page.eval(`[...document.querySelectorAll('.sidebar-foot button')].find((b) => b.textContent === 'Spend').click()`);
  check(await page.eval(`document.querySelector('dialog.spend').open && /Still reading/.test(document.querySelector('dialog.spend [role=status]')?.textContent ?? '')`), 'Spend before the history is read says it is still reading, not an empty total');
  await press(page, 'Escape');

  await open(connected, { mode: 'forbidden' });
  await page.waitFor(`document.querySelector('.status')?.dataset.tone === 'error'`);
  check(await page.eval(`!!document.querySelector('[role=alert] button') && document.querySelector('[role=alert]').textContent.includes('sign-in link')`), 'a rejected token stops with a reason, the sign-in hint and a Reload button');
  await axe('stopped');
  await shoot('error-dark');

  await open(connected, { mode: 'trimmed' });
  await page.waitFor(`document.querySelector('.status')?.dataset.tone === 'error'`);
  check(await page.eval(`!document.querySelector('[role=alert] button') && !document.querySelector('[role=alert]').textContent.includes('sign-in')`), 'a stop that is not about the token gives no sign-in advice');

  await open(connected, { mode: 'down' });
  await page.waitFor(`document.querySelector('.status')?.textContent.includes('Retrying')`);
  check(await page.eval(`document.querySelector('.empty h2')?.textContent === 'Cannot reach the gateway'`), 'a gateway that is down at open says nothing has been read, not that there is no work');
  await axe('unreachable');
  await shoot('unreachable-dark');

  await open(connected, { width: 390, height: 844 });
  await page.waitFor(live);
  await axe('phone');
  await shoot('phone-dark');
  await press(page, 'End');
  await press(page, 'Enter');
  const panelState = `JSON.stringify((() => { const a = document.activeElement; const r = document.querySelector('.panel').getBoundingClientRect(); return { panel: !!a?.matches('.panel'), active: a ? a.tagName + '.' + a.className : null, top: Math.round(r.top), height: Math.round(r.height), inner: innerHeight, scrollY: Math.round(scrollY) }; })())`;
  const phoneNow = JSON.parse(await page.eval(panelState));
  await new Promise((ok) => setTimeout(ok, 300));
  const phoneLater = JSON.parse(await page.eval(panelState));
  check(
    phoneNow.panel && phoneNow.top >= 0 && phoneNow.top < phoneNow.inner,
    `on a phone, opening an item moves focus to the panel and scrolls it into view (${JSON.stringify({ now: phoneNow, later: phoneLater })})`,
  );
  await press(page, 'Escape');
  check(await page.eval(`getComputedStyle(document.querySelector('.menu-button')).display !== 'none' && getComputedStyle(document.querySelector('#sidebar')).display === 'none'`), 'on a phone the sidebar is behind a Menu button');
  await page.eval(`document.querySelector('.menu-button').focus(); true`);
  await press(page, 'Enter');
  check(await page.eval(`document.querySelector('.menu-button').getAttribute('aria-expanded') === 'true' && document.querySelector('#sidebar').contains(document.activeElement)`), 'Menu opens the sidebar and moves focus into it');
  check(await page.eval(`[...document.querySelectorAll('#sidebar button')].some((b) => b.textContent.startsWith('Shortcuts'))`), 'Shortcuts and Commands are reachable on a phone (WCAG 3.2.6)');
  check(await page.eval(`document.querySelector('.shell > .main').inert && getComputedStyle(document.querySelector('.drawer-backdrop')).display === 'block'`), 'the open drawer has a backdrop and the page behind it is inert');
  await press(page, 'Tab', 8);
  check(await page.eval(`document.querySelector('#sidebar').contains(document.activeElement)`), 'Shift+Tab from the first stop wraps inside the drawer');
  await press(page, 'Tab');
  check(await page.eval(`document.querySelector('#sidebar').contains(document.activeElement)`), 'Tab from the last stop wraps inside the drawer');
  await axe('phone menu');
  await shoot('phone-menu-dark');
  await press(page, 'Escape');
  check(await focused('.menu-button') && (await page.eval(`document.querySelector('.menu-button').getAttribute('aria-expanded') === 'false' && !document.querySelector('.shell > .main').inert`)), 'Esc closes the menu, gives focus back to the button, and the page is live again');
  await open(connected, { width: 390, height: 844, storage: { 'kbl.dashboard.views': '[{"name":"Tier 2","search":"tier=2"}]' } });
  await page.waitFor(live);
  await page.eval(`document.querySelector('.menu-button').click(); true`);
  await page.eval(`document.querySelector('.saved-row button').focus(); true`);
  await press(page, 'Enter');
  check(await page.eval(`document.querySelector('.view-dialog').open && document.querySelector('.menu-button').getAttribute('aria-expanded') === 'false'`), 'on a phone, Edit closes the drawer before its dialog opens');
  await press(page, 'Escape');
  check(await focused('.menu-button'), 'cancelling the dialog returns focus to Menu');

  // Writes: the inbox, sign-in, answer, tier with undo, pending, a refusal, bulk accept and close.
  await open(`${connected}&view=inbox`);
  await page.waitFor(live);
  check((await page.eval(`[...document.querySelectorAll('.row')].map((r) => r.dataset.id).join(',')`)) === 'gh-2204', 'the Needs you inbox lists only the items waiting on a person');
  check(await page.eval(`document.querySelector('.row .row-waits').textContent === 'An agent asked a question'`), 'an inbox row says why it needs you');
  check(await page.eval(`document.querySelector('.nav a[data-view=inbox]').textContent.includes('1')`), 'the sidebar counts what needs you');
  await axe('inbox');
  await shoot('inbox-dark');
  await page.eval(`document.querySelector('.row[data-id=gh-2204]').click(); true`);
  check(await page.eval(`document.querySelector('.actions [data-focus=sign-in]') !== null`), 'signed out, the panel offers Sign in instead of actions');
  await axe('actions signed out');
  await page.eval(`document.querySelector('.actions [data-focus=sign-in]').click(); true`);
  check(await focused('#tracker-token'), 'Sign in asks for the person token, with focus in the field');
  await page.eval(`document.querySelector('.sign-in-dialog form').requestSubmit(); true`);
  check(await page.eval(`document.querySelector('#tracker-token-error').textContent === 'Paste your person token.' && document.querySelector('#tracker-token').getAttribute('aria-invalid') === 'true'`), 'an empty token is named next to the field');
  await page.eval(`document.querySelector('#tracker-token').value = 'wrong'; document.querySelector('.sign-in-dialog form').requestSubmit(); true`);
  await page.waitFor(`document.querySelector('#tracker-token-error').textContent.includes('does not know')`);
  check(true, 'a token the tracker refuses shows the tracker’s reason');
  await axe('sign in error');
  await shoot('sign-in-dark');
  await page.eval(`document.querySelector('#tracker-token').value = 'person-token'; document.querySelector('.sign-in-dialog form').requestSubmit(); true`);
  await page.waitFor(`!document.querySelector('.sign-in-dialog').open`);
  // Changing a price in effect: Save waits for the confirm, and a price changed underneath asks again.
  const row = `document.querySelector('dialog.prices button[data-price-row="claude-opus-5-5|input"]')`;
  const saveOff = `document.querySelector('dialog.prices form.price-edit .button:not(.ghost)').disabled`;
  const typePrice = (v) => page.eval(`(() => { const i = document.querySelector('#price-input'); i.value = ${JSON.stringify(v)}; i.dispatchEvent(new Event('input', { bubbles: true })); return true; })()`);
  await page.eval(`[...document.querySelectorAll('.sidebar-foot button')].find((b) => b.textContent === 'Spend').click(); [...document.querySelectorAll('dialog.spend button')].find((b) => b.textContent === 'Models and prices').click(); ${row}.click(); true`);
  check(await focused('#price-input'), 'Change moves focus into the price field');
  await typePrice('20');
  check(await page.eval(saveOff), 'Save stays disabled until the confirm is ticked');
  await page.eval(`document.querySelector('#price-confirm').click(); true`);
  check(!(await page.eval(saveOff)), 'ticking the confirm enables Save');
  await page.eval(`document.querySelector('#price-confirm').click(); true`);
  check(await page.eval(saveOff), 'unticking the confirm disables Save again');
  await page.eval(`document.querySelector('#price-confirm').click(); window.__push({ v: 8, actor: 'person', by: 'maintainer', at_ms: Date.now(), kind: 'price_set', model: 'claude-opus-5-5', unit: 'input', price_micros: 17_000_000 }); true`);
  await new Promise((ok) => setTimeout(ok, 300));
  const calls = await page.eval(`window.__tracker.calls.length`);
  await page.eval(`document.querySelector('dialog.prices form.price-edit .button:not(.ghost)').click(); true`);
  check((await page.eval(`window.__tracker.calls.length`)) === calls, 'a price changed underneath is not posted on the old confirm');
  check(await page.eval(`!document.querySelector('#price-confirm').checked && ${saveOff} && document.querySelector('dialog.prices form.price-edit [role=status]').textContent.includes('$17.00')`), 'it shows the new old-to-new change and asks for the confirm again');
  await page.eval(`document.querySelector('#price-confirm').click(); document.querySelector('dialog.prices form.price-edit .button:not(.ghost)').click(); true`);
  await page.waitFor(`window.__tracker.calls.at(-1).url === '/v1/work/prices'`);
  check(await page.eval(`window.__tracker.calls.at(-1).body.price_micros === 20_000_000`), 'the confirmed change is posted');
  await new Promise((ok) => setTimeout(ok, 300));
  check(await page.eval(`document.querySelector('dialog.prices [role=status]:last-child')?.textContent.startsWith('Price set for claude-opus-5-5')`), 'the saved message stays on the page after the redraw');
  check(await page.eval(`document.activeElement === ${row}`), 'after Save focus returns to the row’s Change button');
  await page.eval(`${row}.click(); true`);
  await page.eval(`[...document.querySelectorAll('dialog.prices form.price-edit .button.ghost')].find((b) => b.textContent === 'Cancel').click(); true`);
  check(await page.eval(`document.activeElement === ${row}`), 'Cancel returns focus to the row’s Change button');
  await press(page, 'Escape');
  check(await page.eval(`!!document.querySelector('.actions form.answer') && document.querySelector('.answer .question').textContent === 'Which client saw it?'`), 'signed in, a question shows its recorded text and an answer form');
  check(await page.eval(`[...document.querySelectorAll('.answer .choice')].map((c) => c.textContent).join('|') === 'Rust client (the agent’s default)|Python client'`), 'the options come from the question, with its default named');
  await page.eval(`document.querySelector('.answer').requestSubmit(); true`);
  check(await page.eval(`document.querySelector('#answer-error').textContent === 'Pick an option, or write an answer.'`) && (await focused('#answer-text')), 'an empty answer is named next to the field and the focus goes there');
  await page.eval(`document.querySelector('#answer-text').value = 'half typed'; true`);
  await page.eval(`window.__push({ actor: 'person', by: 'maintainer', at_ms: 1, kind: 'context', item: 'gh-2204', entry: 'notes', text: { text: 'a live note' } }); true`);
  await new Promise((ok) => setTimeout(ok, 300));
  check((await focused('#answer-text')) && (await page.eval(`document.querySelector('#answer-text').value === 'half typed'`)), 'a live record keeps a half-typed answer and the focus in it');
  await axe('answer form');
  await shoot('answer-dark');
  const sent = await page.eval(`window.__tracker.calls.length`);
  await page.eval(`document.querySelector('.answer input[value="Python client"]').click(); document.querySelector('.answer').requestSubmit(); true`);
  check((await page.eval(`document.querySelector('#answer-error').textContent.includes('not both')`)) && (await page.eval(`window.__tracker.calls.length`)) === sent, 'an option and text together are named before sending, since the tracker takes one');
  await page.eval(`document.querySelector('#answer-text').value = ''; document.querySelector('.answer').requestSubmit(); true`);
  await page.waitFor(`document.querySelector('.activity-text').textContent === 'Answered gh-2204.'`);
  const answered = await page.eval(`JSON.stringify(window.__tracker.calls.at(-1))`);
  check(answered.includes('"url":"/v1/work/questions/answer"') && answered.includes('"auth":"Bearer person-token"') && answered.includes('"chosen":"Python client"'), `the answer goes to the person route with the viewer’s token (${answered})`);
  await page.waitFor(`document.querySelectorAll('.row').length === 0`);
  check(await page.eval(`document.querySelector('.empty h2')?.textContent === 'Nothing needs you'`), 'answered, the item leaves the inbox and the inbox says it is empty');

  await page.eval(`document.querySelector('.nav a[data-view=all]').click(); document.querySelector('.row[data-id=gh-2219]').click(); true`);
  const tierOf = `[...document.querySelectorAll('.panel dt')].find((d) => d.textContent === 'Tier')?.nextElementSibling.textContent`;
  await page.eval(`document.querySelector('#panel-tier').value = '3'; document.querySelector('[data-focus=set-tier]').focus(); document.querySelector('[data-focus=set-tier]').click(); true`);
  await page.waitFor(`document.querySelector('.activity-text').textContent === 'Set the tier of gh-2219.'`);
  // The tracker answers before the record comes back on the log, so wait for the fold to show it.
  await page.waitFor(`${tierOf} === '3'`);
  check((await page.eval(tierOf)) === '3' && (await page.eval(`!document.querySelector('.activity button').hidden`)), 'a tier change is written, shown, and offers Undo');
  await axe('tier set');
  await press(page, 'z', MOD);
  await page.waitFor(`document.querySelector('.activity-text').textContent === 'Undid the tier change on gh-2219.'`);
  await page.waitFor(`${tierOf} === '1'`);
  check((await page.eval(tierOf)) === '1' && (await page.eval(`JSON.stringify(window.__tracker.calls.at(-1).body) === '{"item":"gh-2219","tier":1}'`)), `${MOD_NAME}+Z undoes it with a new tier_changed record setting the old tier back`);
  await shoot('undo-dark');
  await page.eval(`window.__tracker.delay = 800; document.querySelector('#panel-tier').value = '0'; document.querySelector('[data-focus=set-tier]').click(); true`);
  await new Promise((ok) => setTimeout(ok, 150));
  check(await page.eval(`document.querySelector('.actions').hasAttribute('aria-busy') && document.querySelector('.write-status').textContent === 'Setting the tier…' && document.querySelector('.activity-text').textContent === 'Setting the tier of gh-2219…'`), 'a write in flight shows as pending in the panel and the activity line');
  check(await focused('[data-focus=set-tier]'), 'the pending button keeps the focus (aria-disabled, not disabled)');
  await axe('write pending');
  await shoot('pending-dark');
  await page.waitFor(`document.querySelector('.activity-text').textContent === 'Set the tier of gh-2219.'`);
  await page.eval(`window.__tracker.delay = 0; window.__tracker.refuse['gh-2221'] = { error: 'the fold refused the record: the item is closed', why: 'closed' }; document.querySelector('.row[data-id=gh-2221]').click(); true`);
  await page.eval(`document.querySelector('#panel-tier').value = '0'; document.querySelector('[data-focus=set-tier]').click(); true`);
  await page.waitFor(`document.querySelector('.activity').dataset.tone === 'error'`);
  check(await page.eval(`document.querySelector('.activity-text').textContent === 'gh-2221: refused, the fold refused the record: the item is closed (closed)' && document.querySelector('.write-status').textContent.includes('the item is closed (closed)')`), 'a refused write shows the tracker’s reason in the activity line and the panel');
  await axe('write refused');
  await shoot('refused-dark');

  await press(page, 'Escape');
  await page.eval(`document.querySelector('.row[data-id=ado-4377]').focus(); true`);
  await press(page, 'x');
  await page.eval(`document.querySelector('.row[data-id=ado-4380]').focus(); true`);
  await press(page, 'x');
  check(await page.eval(`document.querySelector('.bulk-count').textContent === '2 selected' && document.querySelectorAll('.mark:checked').length === 2`), 'X marks the focused row, and the bulk bar counts the marks');
  check(await page.eval(`[...document.querySelectorAll('.bulk button')].filter((b) => !b.hidden).map((b) => b.textContent).join('|') === 'Set tier|Accept 2…|Close 2…|Clear selection'`), 'the bulk bar offers only what the marked items allow');
  await axe('bulk');
  await shoot('bulk-dark');
  await page.eval(`[...document.querySelectorAll('.bulk button')].find((b) => b.textContent.startsWith('Accept')).click(); true`);
  check(await page.eval(`document.querySelector('.confirm-dialog').open && document.querySelector('#confirm-title').textContent === 'Accept 2 items?' && document.querySelector('#confirm-undo').textContent.startsWith('This cannot be undone.')`), 'accepting asks to confirm, lists what it covers, and says it cannot be undone');
  check(await focused('#confirm-tier'), 'the confirm dialog opens on its tier field');
  await axe('confirm');
  await shoot('confirm-dark');
  const before = await page.eval(`window.__tracker.calls.length`);
  await press(page, 'Escape');
  check((await page.eval(`window.__tracker.calls.length`)) === before, 'cancelling sends nothing');
  await page.eval(`[...document.querySelectorAll('.bulk button')].find((b) => b.textContent.startsWith('Accept')).click(); document.querySelector('.confirm-dialog form').requestSubmit(); true`);
  await page.waitFor(`document.querySelector('.activity-text').textContent === 'Accepted 2 items.'`);
  check(await page.eval(`['ado-4377', 'ado-4380'].every((id) => document.querySelector('#group-ready').closest('section').querySelector('[data-id=' + id + ']'))`), 'bulk accept writes one record per item and both move to Ready');
  check(await page.eval(`document.querySelector('.activity button').hidden`), 'an accept offers no Undo, since no record takes it back');
  check(await page.eval(`window.__tracker.calls.slice(-2).every((c) => c.url.endsWith('/proposals/accept') && !('tier' in c.body))`), 'a bulk accept keeps each item’s own tier by default');
  check(await page.eval(`document.querySelector('.bulk').hidden && document.querySelectorAll('.mark:checked').length === 0`), 'the marks clear once their action is sent');
  await page.eval(`document.querySelector('.row[data-id=ado-4377]').focus(); true`);
  await press(page, 'x');
  check(await page.eval(`!document.querySelector('.bulk').hidden`), 'X marks again after a bulk action');
  await press(page, 'Escape');
  check(await page.eval(`document.querySelector('.bulk').hidden && document.querySelectorAll('.mark:checked').length === 0`), 'Esc clears the selection');
  await page.eval(`document.querySelector('.mark[data-mark=gh-2210]').click(); [...document.querySelectorAll('.bulk button')].find((b) => b.textContent.startsWith('Close')).click(); true`);
  check(await page.eval(`document.querySelector('#confirm-why') && document.querySelector('#confirm-undo').textContent.includes('children stay cancelled')`), 'closing asks why and says what cannot be undone');
  await page.eval(`document.querySelector('#confirm-why').value = 'completed'; document.querySelector('.confirm-dialog form').requestSubmit(); true`);
  await page.waitFor(`document.querySelector('.activity-text').textContent === 'Closed gh-2210.'`);
  await page.waitFor(`!!document.querySelector('#group-done')?.closest('section').querySelector('[data-id=gh-2210]')`);
  check(await page.eval(`!!document.querySelector('#group-done')?.closest('section').querySelector('[data-id=gh-2210]')`), 'a checkbox marks a row, and the close lands as completed');
  // A Triage item keeps triage's tier: the panel and the confirm start from it, and the accept sends the tier shown.
  const by = `actor: 'person', by: 'maintainer', at_ms: 1800000000500`;
  await page.eval(`window.__push({ kind: 'imported', item: 'tri-9', source: 'jira', upstream_id: 'FEL-9', title: 'Urgent triage', ${by} }); window.__push({ kind: 'triaged', item: 'tri-9', tier: 0, why: 'urgent', ${by} }); true`);
  await page.waitFor(`!!document.querySelector('.row[data-id=tri-9]')`);
  await page.eval(`document.querySelector('.row[data-id=tri-9]').click(); true`);
  await page.waitFor(`document.querySelector('.panel-head .row-id')?.textContent === 'tri-9' && !!document.querySelector('#panel-tier')`);
  check(await page.eval(`document.querySelector('#panel-tier').value === '0'`), 'the panel shows the tier triage proposed, not a default');
  await page.eval(`document.querySelector('.actions [data-focus=accept]').click(); true`);
  check(await page.eval(`document.querySelector('#confirm-tier').value === '0'`), 'accepting a Triage item starts from its proposed tier');
  await page.eval(`document.querySelector('.confirm-dialog form').requestSubmit(); true`);
  await page.waitFor(`document.querySelector('.activity-text').textContent === 'Accepted tri-9.'`);
  check(await page.eval(`window.__tracker.calls.at(-1).body.item === 'tri-9' && window.__tracker.calls.at(-1).body.tier === 0`), 'an accept left at the proposed tier sends that tier, the one the confirm showed');
  await page.waitFor(`${tierOf} === '0'`);
  check((await page.eval(tierOf)) === '0', 'the accepted item has the tier triage proposed');

  // An item with no tier shows none, and a first tier says it has no Undo.
  await page.eval(`window.__push({ kind: 'imported', item: 'tri-7', source: 'jira', upstream_id: 'FEL-7', title: 'No tier yet', ${by} }); true`);
  await page.waitFor(`!!document.querySelector('.row[data-id=tri-7]')`);
  await page.eval(`document.querySelector('.row[data-id=tri-7]').click(); true`);
  await page.waitFor(`document.querySelector('.panel-head .row-id')?.textContent === 'tri-7' && !!document.querySelector('#panel-tier')`);
  check(await page.eval(`document.querySelector('#panel-tier').value === '' && document.querySelector('#panel-tier').selectedOptions[0].textContent === 'No tier recorded'`), 'an item with no recorded tier shows none in the panel');
  await page.eval(`document.querySelector('#panel-tier').value = '1'; document.querySelector('[data-focus=set-tier]').click(); true`);
  await page.waitFor(`document.querySelector('.activity-text').textContent.startsWith('Set the tier of tri-7.')`);
  check(await page.eval(`document.querySelector('.activity-text').textContent === 'Set the tier of tri-7. No undo for tri-7: it had no tier before, and no record removes a tier.' && document.querySelector('.activity button').hidden`), 'a first tier says it has no Undo');

  // Redo from a decision: a button on the decision and a palette command open a confirm that names the
  // decision and the redos left; the note is written first and the redo names it; the new attempt shows its fork point.
  const agentRec = (r) => page.eval(`window.__push({ actor: 'agent', by: 'agent-1', at_ms: Date.now(), item: 'rd-1', attempt: 1, ...${JSON.stringify(r)} })`);
  await page.eval(`window.__push({ kind: 'imported', item: 'rd-1', source: 'github', upstream_id: 'GetFelix/KBL#9100', title: 'Redo me', ${by} }); window.__push({ kind: 'accepted', item: 'rd-1', tier: 1, ${by} }); true`);
  await agentRec({ kind: 'claimed', deadline_ms: Date.now() + 3_600_000 });
  await agentRec({ kind: 'decision', phase: 'fix', so_far: 'reading', question: 'how', chosen: 'guard the read', why: 'tests', commit: 'c1', evidence: [] });
  const redoDecision = await page.eval('window.__ws.liveAt - 1');
  await agentRec({ v: 8, kind: 'done', usage: [{ unit: 'tokens', model: 'claude-opus-5-5', input: 1000 }] });
  await page.waitFor(`!!document.querySelector('.row[data-id=rd-1]')`);
  await page.eval(`document.querySelector('.row[data-id=rd-1]').click(); true`);
  await page.waitFor(`!!document.querySelector('.panel .story-decision [data-focus="redo:${redoDecision}"]')`);
  const redoBtn = await page.eval(`document.querySelector('[data-focus="redo:${redoDecision}"]').getAttribute('aria-label')`);
  check(redoBtn === `Redo from decision ${redoDecision} on attempt 1`, `a decision offers Redo from here with an accessible name (saw "${redoBtn}")`);
  await page.eval(`document.querySelector('[data-focus="redo:${redoDecision}"]').click(); true`);
  const copy = await page.eval(`document.querySelector('.redo-dialog')?.textContent ?? ''`);
  console.log(`saw redo dialog: ${copy}`);
  check(copy.includes(`from decision ${redoDecision}`) && copy.includes('counts against the redos') && /\d left now/.test(copy), 'the redo confirm names the decision, the budget and what is left');
  check(await focused('#redo-note'), 'the redo confirm puts focus in the optional note');
  await axe('redo confirm');
  await page.eval(`document.querySelector('#redo-note').value = 'use the lock instead'; document.querySelector('.redo-dialog form').requestSubmit(); true`);
  await page.waitFor(`document.querySelector('.activity-text').textContent.startsWith('Started a redo of rd-1')`);
  const redoCalls = await page.eval(`JSON.stringify(window.__tracker.calls.slice(-2).map((c) => [c.url, c.body]))`);
  console.log(`saw redo calls: ${redoCalls}`);
  check(await page.eval(`(() => { const [n, r] = window.__tracker.calls.slice(-2); return n.url === '/v1/work/note' && r.url === '/v1/work/redo' && r.body.note === window.__ws.liveAt - 2 && r.body.at === ${redoDecision} && r.body.from_attempt === 1; })()`), 'the note is written first and the redo names it, the decision and its attempt');
  await agentRec({ kind: 'claimed', deadline_ms: Date.now() + 3_600_000, attempt: 2 });
  await agentRec({ kind: 'decision', phase: 'fix', so_far: 'again', question: 'how', chosen: 'take the lock', why: 'redo note', commit: 'c2', evidence: [], attempt: 2 });
  await page.waitFor(`!!document.querySelector('.story-forks li')`);
  const fork = await page.eval(`document.querySelector('.story-forks li').textContent`);
  console.log(`saw fork line: ${fork}`);
  check(fork.startsWith(`Attempt 2 is a redo from decision ${redoDecision} on attempt 1.`), 'the new attempt is linked to its fork point in the panel');
  await page.eval(`document.querySelector('.story-forks .story-ref').click(); true`);
  check(await focused(`.story-decision[data-at="${redoDecision}"]`), 'the fork link moves the focus to the decision it branched from');
  await press(page, 'r');
  const blocked = await page.eval(`document.querySelector('.redo-dialog')?.open ? document.querySelector('.redo-dialog').textContent : ''`);
  console.log(`saw redo while an agent holds the item: ${blocked}`);
  check(blocked.includes('Cannot redo rd-1 now') && blocked.includes('An agent holds the item'), 'R while an agent holds the item says why it cannot redo instead of sending');
  await page.eval(`document.querySelector('.redo-dialog').close(); true`);

  // Triage's own question: the form asks what triage could not settle.
  await page.eval(`window.__push({ kind: 'imported', item: 'tri-8', source: 'jira', upstream_id: 'FEL-8', title: 'Which repo', ${by} }); window.__push({ kind: 'triaged', item: 'tri-8', tier: 1, unclear: 'repo', why: 'which repo', ${by} }); true`);
  await page.waitFor(`!!document.querySelector('.row[data-id=tri-8]')`);
  await page.eval(`document.querySelector('.row[data-id=tri-8]').click(); true`);
  await page.waitFor(`document.querySelector('.panel-head .row-id')?.textContent === 'tri-8' && !!document.querySelector('.answer .question')`);
  check(await page.eval(`document.querySelector('.answer .question').textContent === 'Triage could not tell which repository the work belongs in. Which one is it?'`), 'an unclear item’s answer form says what triage could not settle');
  await axe('unclear answer');

  // Who is signed in, and Sign out.
  check(await page.eval(`document.querySelector('.account-who').textContent === 'Signed in as maintainer' && document.querySelector('.account-button').textContent === 'Sign out'`), 'the sidebar names who is signed in');
  await page.eval(`document.querySelector('.account-button').click(); true`);
  await page.waitFor(`!!document.querySelector('.actions [data-focus=sign-in]')`);
  check(await page.eval(`document.querySelector('.account-who').textContent === 'Viewing (read-only)' && document.querySelector('.account-button').textContent === 'Sign in to act'`), 'Sign out forgets the token and the panel asks to sign in');
  await axe('signed out by hand');
  await page.eval(`document.querySelector('.account-button').click(); true`);
  check(await focused('#tracker-token'), 'Sign in from the sidebar asks for the token');
  await page.eval(`document.querySelector('#tracker-token').value = 'person-token'; document.querySelector('.sign-in-dialog form').requestSubmit(); true`);
  await page.waitFor(`!document.querySelector('.sign-in-dialog').open && document.querySelector('.account-button').textContent === 'Sign out'`);
  // Workers: a supervisor and its workers as a group, each worker's hold, a finished worker, and stop for one or all.
  {
    const w = (r) => page.eval(`window.__push(${JSON.stringify({ v: 24, actor: 'agent', at_ms: Date.now(), ...r })}); true`);
    const hash = (c) => c.repeat(64);
    await page.eval(`window.__push({ v: 24, kind: 'format_raised', to: 24, actor: 'person', by: 'maintainer', at_ms: Date.now() }); window.__push({ kind: 'imported', item: 'wk-1', source: 'github', title: 'Held by a worker', upstream_id: 'GetFelix/felix#7001', actor: 'tracker', by: 'ingest', at_ms: Date.now() }); window.__push({ kind: 'accepted', item: 'wk-1', tier: 1, actor: 'person', by: 'maintainer', at_ms: Date.now() }); true`);
    for (const [n, c, extra] of [['sup-a', 'a', { vendor: 'anthropic', model: 'sonnet' }], ['sup-b', 'b', { vendor: 'example', profile: 'cloud' }], ['sup-c', 'c', {}], ['sup-d', 'd', {}]]) {
      await w({ by: 'sup', kind: 'worker_started', worker: n, token_sha256: hash(c), supervisor_token_sha256: hash('f'), ...extra });
    }
    await w({ by: 'sup-a', sup: 'sup', kind: 'claimed', item: 'wk-1', attempt: 1, deadline_ms: Date.now() + 3_600_000 });
    await w({ by: 'sup-c', sup: 'sup', kind: 'worker_finished' });
    await page.eval(`window.__push({ v: 24, kind: 'worker_stopped', worker: 'sup-d', actor: 'person', by: 'maintainer', at_ms: Date.now() }); true`);
    await page.eval(`[...document.querySelectorAll('.sidebar-foot button')].find((b) => b.textContent === 'Workers').click(); true`);
    await page.waitFor(`!!document.querySelector('dialog.workers section[data-supervisor=sup] tr[data-worker=sup-d]')`);
    const rows = await page.eval(`[...document.querySelectorAll('dialog.workers tr[data-worker]')].map((r) => r.dataset.worker + ':' + r.dataset.state).join(' ')`);
    check(rows === 'sup-a:working sup-b:idle sup-c:finished sup-d:stopped', `workers: one group, live workers first, finished and stopped kept (${rows})`);
    check(await page.eval(`document.querySelector('dialog.workers tr[data-worker=sup-a] a[data-item=wk-1]')?.textContent === 'wk-1 Held by a worker'`), 'workers: a working worker names the item it holds');
    check(await page.eval(`document.querySelector('dialog.workers tr[data-worker=sup-a]').textContent.includes('anthropic · sonnet') && document.querySelector('dialog.workers tr[data-worker=sup-c]').textContent.includes('finished')`), 'workers: the recorded launch fields and the finish show');
    await axe('workers');
    await shoot('workers-dark');
    await page.eval(`document.querySelector('dialog.workers [data-stop=sup-a]').click(); true`);
    // The fake tracker logs only the stop, so the worker shows Releasing while it still holds; the tracker's release then ends it as Stopped.
    await page.waitFor(`document.querySelector('dialog.workers tr[data-worker=sup-a]')?.dataset.state === 'releasing'`);
    check(await page.eval(`!document.querySelector('dialog.workers [data-stop=sup-a]')`), 'workers: a worker being released offers no second Stop');
    await page.eval(`window.__push({ kind: 'released', item: 'wk-1', attempt: 1, reason: 'supervisor_gone', actor: 'tracker', by: 'ingest', at_ms: Date.now() }); true`);
    await page.waitFor(`document.querySelector('dialog.workers tr[data-worker=sup-a]')?.dataset.state === 'stopped'`);
    check(await page.eval(`JSON.stringify(window.__tracker.calls.at(-1)) === '{"url":"/v1/work/workers/stop","auth":"Bearer person-token","body":{"worker":"sup-a"}}'`), 'workers: Stop sends that worker to the stop route with the viewer token');
    await page.eval(`document.querySelector('dialog.workers [data-stop-all=sup]').click(); true`);
    check(await page.eval(`!!document.querySelector('dialog.workers [data-confirm-stop=sup]') && !document.querySelector('dialog.workers tr[data-worker=sup-b][data-state=stopped]')`), 'workers: Stop all asks first and stops nothing yet');
    await page.eval(`document.querySelector('dialog.workers [data-confirm-stop=sup]').click(); true`);
    await page.waitFor(`document.querySelector('dialog.workers tr[data-worker=sup-b]')?.dataset.state === 'stopped'`);
    check(await page.eval(`!document.querySelector('dialog.workers [data-stop]') && !document.querySelector('dialog.workers [data-stop-all]')`), 'workers: with none live, no stop control is left');
    await press(page, 'Escape');
  }
  await page.eval(`window.__tracker.token = 'rotated'; document.querySelector('.row[data-id=gh-2219]').click(); document.querySelector('[data-focus=set-tier]').click(); true`);
  await page.waitFor(`!!document.querySelector('.actions [data-focus=sign-in]')`);
  check(await page.eval(`document.querySelector('.actions p').textContent.startsWith('Sign in again: the tracker does not know this token')`), 'a token the tracker stops taking signs the page out, with the reason');
  await shoot('signed-out-dark');
  clean();

  // Landmarks and live regions: one main, named navigation, one h1, and announcers that exist before they speak.
  await open(connected);
  await page.waitFor(live);
  const landmarks = await page.eval(`({
    main: document.querySelectorAll('main, [role=main]').length,
    h1: document.querySelectorAll('h1').length,
    unnamedNav: [...document.querySelectorAll('nav')].filter((n) => !n.getAttribute('aria-label') && !n.getAttribute('aria-labelledby')).length,
    nav: document.querySelectorAll('nav').length,
    live: document.querySelectorAll('[aria-live], [role=status], [role=alert]').length,
  })`);
  check(landmarks.main === 1 && landmarks.h1 === 1, `landmarks: one main and one h1 (${landmarks.main}, ${landmarks.h1})`);
  check(landmarks.nav >= 1 && landmarks.unnamedNav === 0, `landmarks: every nav has a name (${landmarks.unnamedNav} of ${landmarks.nav} unnamed)`);
  check(landmarks.live >= 2, `live regions: the status line and the announcer are in the page (${landmarks.live})`);
  check(await page.eval(`document.querySelector('.status').getAttribute('aria-live') === 'polite'`), 'live regions: the connection status is polite, not assertive');

  // Keyboard: Tab walks the page in order, each stop shows a focus indicator, and nothing is a trap.
  await page.eval(`document.activeElement?.blur(); window.scrollTo(0, 0); true`);
  const stops = [];
  for (let i = 0; i < 80; i++) {
    await press(page, 'Tab');
    const stop = await page.eval(`(() => {
      const el = document.activeElement;
      if (!el || el === document.body) return null;
      const seen = (e) => { const c = getComputedStyle(e); return (c.outlineStyle !== 'none' && parseFloat(c.outlineWidth) > 0) || c.boxShadow !== 'none'; };
      // A search field draws its ring on the wrapper that contains the focus.
      const ring = seen(el) || (el.closest('.search') ? seen(el.closest('.search')) : false);
      const r = el.getBoundingClientRect();
      return { name: el.tagName.toLowerCase() + (el.className ? '.' + String(el.className).split(' ')[0] : ''), ring, onscreen: r.width > 0 && r.height > 0 };
    })()`);
    if (!stop) break;
    stops.push(stop);
    if (stops.length > 1 && stop.name === stops[0].name && stops.length > 3 && (await focused('.skip'))) break;
  }
  const noRing = stops.filter((st) => !st.ring).map((st) => st.name);
  const hidden = stops.filter((st) => !st.onscreen).map((st) => st.name);
  check(stops.length >= 5, `keyboard: Tab reaches the page's controls (${stops.length} stops)`);
  check(noRing.length === 0, `keyboard: every Tab stop shows a focus indicator${noRing.length ? ` (missing on ${[...new Set(noRing)].join(', ')})` : ''}`);
  check(hidden.length === 0, `keyboard: no Tab stop is an invisible element${hidden.length ? ` (${[...new Set(hidden)].join(', ')})` : ''}`);
  clean();

  // Group by: headers are buttons, folding hides rows and j/k skip them, the choice is in the address.
  {
    const seen = (js) => page.eval(js);
    await open(`${connected}&group=tier`);
    await page.waitFor(live);
    const names = await seen(`[...document.querySelectorAll('.list .group-toggle')].map((b) => b.querySelector('.group-name').textContent + ' ' + b.querySelector('.count').textContent).join(' | ')`);
    console.log(`     group by tier: ${names}`);
    check(await seen(`document.querySelector('#group-by').value === 'tier' && document.querySelectorAll('.list .group-toggle').length > 1`), 'group by: ?group=tier groups the list by tier and the toolbar shows it');
    check(await seen(`[...document.querySelectorAll('.list .group-toggle')].every((b) => b.getAttribute('aria-expanded') === 'true' && /^(Tier \\d+|No tier)$/.test(b.querySelector('.group-name').textContent))`), 'group by: each header is a button, open, named for its tier');
    const rowsBefore = await seen(`document.querySelectorAll('.list .row').length`);
    const firstCount = Number(await seen(`document.querySelector('.list .group-toggle .count').textContent`));
    await seen(`document.querySelector('.list .group-toggle').focus(); document.querySelector('.list .group-toggle').click(); true`);
    const rowsAfter = await seen(`document.querySelectorAll('.list .row').length`);
    console.log(`     folded the first group: ${rowsBefore} rows -> ${rowsAfter}, count ${firstCount}`);
    check(rowsAfter === rowsBefore - firstCount, 'group by: folding a group hides its rows and keeps its count');
    check(await seen(`document.querySelector('.list .group-toggle').getAttribute('aria-expanded') === 'false'`), 'group by: a folded header says aria-expanded false');
    check(await focused('.list .group-toggle'), 'group by: the header keeps the focus after folding');
    let stray = 0;
    for (let n = 0; n < rowsAfter + 1; n++) {
      await press(page, 'j');
      if (!(await seen(`(() => { const r = document.querySelector('.list .row[data-selected]'); return !!r && r.closest('section').querySelector('.group-toggle').getAttribute('aria-expanded') === 'true'; })()`))) stray++;
    }
    check(stray === 0, `group by: j never selects a row in a folded group (${stray} strays over ${rowsAfter + 1} presses)`);
    await seen(`document.querySelector('.list .row[data-selected]').focus(); true`);
    await seen(`(() => { const s = document.querySelector('#group-by'); s.value = 'repo'; s.dispatchEvent(new Event('change')); return true; })()`);
    check(await seen(`document.activeElement?.classList.contains('row') && document.activeElement.hasAttribute('data-selected')`), 'group by: regrouping keeps the focus on the same row');
    await page.waitFor(`location.search.includes('group=repo')`);
    check(await seen(`[...document.querySelectorAll('.list .group-name')].some((n) => n.textContent === 'GetFelix/felix')`), 'group by: picking repo regroups the list and writes group=repo to the address');
    await seen(`(() => { const s = document.querySelector('#group-by'); s.value = 'none'; s.dispatchEvent(new Event('change')); return true; })()`);
    check(await seen(`document.querySelectorAll('.list .group-toggle').length === 0 && document.querySelectorAll('.list .row').length > 0`), 'group by: None is one list with no headers');
    await open(`${connected}&group=assignee`);
    await page.waitFor(live);
    check(await seen(`document.querySelector('#group-by').value === 'assignee'`), 'group by: a link restores the grouping');
    await open(`${connected}&layout=board&group=tier`);
    await page.waitFor(live);
    check(await seen(`!document.querySelector('#group-by').disabled && document.querySelector('#group-by').value === 'tier' && document.querySelectorAll('.column').length > 1`), 'group by: the board keeps its stage columns and groups them into lanes by the chosen key');
    await axe('group by');
    clean();
  }

  // The Needs-you inbox answers in place: a question by its option key, a triage proposal by Accept.
  {
    const log = fixtureLog();
    let at = 1_900_000_000_000;
    const rec = (r) => log.push({ actor: 'person', by: 'maintainer', at_ms: at++, ...r });
    const agent = (r) => rec({ actor: 'agent', by: 'agent-1', ...r });
    rec({ kind: 'imported', item: 'inb-1', source: 'github', title: 'Which retry policy?', upstream_id: 'GetFelix/felix#9101' });
    rec({ kind: 'triaged', item: 'inb-1', tier: 1, why: 'seen' });
    rec({ kind: 'accepted', item: 'inb-1', tier: 1 });
    agent({ kind: 'claimed', item: 'inb-1', attempt: 1, deadline_ms: at + 3_600_000 });
    agent({ kind: 'question', item: 'inb-1', attempt: 1, text: { text: 'Retry forever or give up after five?' }, options: ['Forever', 'Five tries'], default: 'Five tries' });
    rec({ kind: 'imported', item: 'inb-2', source: 'github', title: 'Slow compaction on cold start', upstream_id: 'GetFelix/felix#9102' });
    rec({ kind: 'triaged', item: 'inb-2', tier: 2, why: 'Compaction waits on the index rebuild' });
    // Opening a view link forgets any person token (session.ts), so sign in through the dialog.
    await open(`${connected}&view=inbox`, { log });
    await page.waitFor(live);
    await page.eval(`document.querySelector('.account-button').click(); true`);
    await page.waitFor(`document.querySelector('.sign-in-dialog')?.open === true`);
    await page.eval(`document.querySelector('#tracker-token').value = 'person-token'; document.querySelector('.sign-in-dialog form').requestSubmit(); true`);
    await page.waitFor(`document.querySelector('.account-who')?.textContent === 'Signed in as maintainer'`);
    const inboxRows = await page.eval(`[...document.querySelectorAll('.row')].map((r) => r.dataset.id).join(',')`);
    const inboxKinds = await page.eval(`[...document.querySelectorAll('.kinds .kind')].map((b) => b.textContent).join('|')`);
    const inboxQuestion = await page.eval(`[document.querySelector('.inbox-act[data-kind=question] .inbox-question')?.textContent, [...document.querySelectorAll('.inbox-act[data-kind=question] .inbox-options button')].map((b) => b.textContent).join('|')].join(' / ')`);
    // The shared fixture already has one question waiting (gh-2204), oldest of all.
    check(inboxRows === 'gh-2204,inb-1,inb-2', `the inbox lists the questions and the triage proposal, oldest first (${inboxRows}; kinds ${inboxKinds}; question ${inboxQuestion})`);
    check(await page.eval(`document.querySelector('.inbox-item:has(.row[data-id=inb-1]) .inbox-act[data-kind=question] .inbox-question').textContent === 'Retry forever or give up after five?' && [...document.querySelectorAll('.inbox-item:has(.row[data-id=inb-1]) .inbox-act[data-kind=question] .inbox-options button')].map((b) => b.textContent).join('|') === '1 Forever|2 Five tries (default)'`), 'a question shows what was asked and its numbered options, the default named');
    check(await page.eval(`[...document.querySelectorAll('.kinds .kind')].map((b) => b.textContent).join('|') === 'Everything3|Questions2|Triage1|Review findings|Decisions'`), 'the sidebar counts each kind');
    await axe('inbox actions');
    await shoot('inbox-actions-dark');
    await page.eval(`[...document.querySelectorAll('.kinds .kind')].find((b) => b.textContent.startsWith('Triage')).click(); true`);
    check((await page.eval(`[...document.querySelectorAll('.row')].map((r) => r.dataset.id).join(',')`)) === 'inb-2', 'a kind narrows the inbox to that kind');
    check(await page.eval(`document.querySelector('.kinds .kind[aria-pressed=true]').textContent.startsWith('Triage')`), 'the chosen kind is marked pressed');
    await page.eval(`[...document.querySelectorAll('.kinds .kind')].find((b) => b.textContent.startsWith('Everything')).click(); true`);
    await page.eval(`document.querySelector('.row[data-id=inb-1]').focus(); true`);
    await press(page, '2');
    await page.waitFor(`window.__tracker.calls.some((c) => c.url === '/v1/work/questions/answer')`);
    check(await page.eval(`window.__tracker.calls.at(-1).body.chosen === 'Five tries' && window.__tracker.calls.at(-1).body.item === 'inb-1'`), 'one key answers a question with the numbered option, from the list');
    await page.waitFor(`[...document.querySelectorAll('.row')].map((r) => r.dataset.id).join(',') === 'gh-2204,inb-2'`);
    check(true, 'the answered item leaves the inbox');
    await page.eval(`document.querySelector('.inbox-item:has(.row[data-id=inb-2]) .inbox-act [data-focus=accept]').click(); true`);
    await page.waitFor(`window.__tracker.calls.at(-1).url === '/v1/work/proposals/accept'`);
    check(await page.eval(`window.__tracker.calls.at(-1).body.item === 'inb-2' && window.__tracker.calls.at(-1).body.tier === 2`), 'Accept in the list sends the shown tier without a confirm');
    await page.waitFor(`[...document.querySelectorAll('.row')].map((r) => r.dataset.id).join(',') === 'gh-2204'`);
    check(true, 'the accepted proposal leaves the inbox');
    clean();
  }

  // Adjusting triage's advice in the inbox row: opened and saved from the keyboard, the draft and the focus
  // surviving a live record, and the list in the order the items began waiting.
  {
    const log = fixtureLog();
    let at = 1_900_100_000_000;
    const rec = (r) => log.push({ actor: 'person', by: 'maintainer', at_ms: at++, ...r });
    rec({ v: 17, kind: 'format_raised', to: 17 });
    for (const [id, n] of [['adv-1', 1], ['adv-2', 2], ['adv-3', 3]]) rec({ kind: 'imported', item: id, source: 'github', title: `Advice ${n}`, upstream_id: `GetFelix/felix#920${n}` });
    // adv-3 is triaged first, so it has waited longest although its id sorts last.
    rec({ kind: 'triaged', item: 'adv-3', tier: 2, why: 'third', depends_on: ['adv-2'], split: ['half', 'other half'] });
    rec({ kind: 'triaged', item: 'adv-1', tier: 1, why: 'first', depends_on: [], split: [] });
    await open(`${connected}&view=inbox`, { log });
    await page.waitFor(live);
    await page.eval(`document.querySelector('.account-button').click(); true`);
    await page.waitFor(`document.querySelector('.sign-in-dialog')?.open === true`);
    await page.eval(`document.querySelector('#tracker-token').value = 'person-token'; document.querySelector('.sign-in-dialog form').requestSubmit(); true`);
    await page.waitFor(`document.querySelector('.account-who')?.textContent === 'Signed in as maintainer'`);
    const order = await page.eval(`[...document.querySelectorAll('.row')].map((r) => r.dataset.id).filter((id) => id.startsWith('adv-')).join(',')`);
    check(order === 'adv-3,adv-1', `the inbox lists proposals in the order triage answered (saw ${order})`);
    const rowOf = (id) => `.inbox-item:has(.row[data-id=${id}])`;
    await page.eval(`document.querySelector('.row[data-id=adv-1]').focus(); true`);
    await press(page, '3');
    check(await page.eval(`!document.querySelector('${rowOf('adv-1')} .inbox-advice').hidden && document.activeElement.dataset.focus === 'advice-deps'`), 'key 3 opens the advice editor and focuses the dependencies field');
    check(await page.eval(`document.querySelector('${rowOf('adv-3')} .inbox-advice').hidden`), 'the editor of another row stays closed (scoped to its own row)');
    await page.eval(`(() => { const d = document.activeElement; d.value = 'adv-2'; d.dispatchEvent(new Event('input', { bubbles: true })); return true; })()`);
    // A live record about another item rebuilds the list; the open editor, its text and the focus stay.
    await page.eval(`window.__push({ actor: 'person', by: 'maintainer', at_ms: Date.now(), kind: 'triaged', item: 'adv-2', tier: 2, why: 'late' }); true`);
    await page.waitFor(`[...document.querySelectorAll('.row')].some((r) => r.dataset.id === 'adv-2')`);
    const kept = await page.eval(`JSON.stringify([document.activeElement.dataset.focus, document.activeElement.value, !document.querySelector('${rowOf('adv-1')} .inbox-advice').hidden])`);
    check(kept === '["advice-deps","adv-2",true]', `a live record keeps the open editor, its text and the focus (saw ${kept})`);
    await page.eval(`document.querySelector('${rowOf('adv-1')} [data-focus=advice-split]').focus(); true`);
    await page.eval(`(() => { const t = document.activeElement; t.value = 'part one\\npart two'; t.dispatchEvent(new Event('input', { bubbles: true })); return true; })()`);
    await axe('advice editor open');
    await press(page, 'Enter', MOD);
    await page.waitFor(`window.__tracker.calls.at(-1).url === '/v1/work/proposals/edit'`);
    const sent = await page.eval(`JSON.stringify(window.__tracker.calls.at(-1).body)`);
    const body = JSON.parse(sent);
    check(body.item === 'adv-1' && body.depends_on.join() === 'adv-2' && body.split.join('|') === 'part one|part two' && typeof body.proposal === 'number', `Ctrl or Cmd+Enter saves the advice for the answer that was read (sent ${sent})`);
    await page.waitFor(`document.querySelector('${rowOf('adv-1')} .inbox-act')?.textContent.includes('Waits on adv-2')`);
    const after = await page.eval(`JSON.stringify([document.querySelector('${rowOf('adv-1')} .inbox-act').textContent.includes('Split into part one, part two'), document.querySelector('${rowOf('adv-1')} .inbox-advice').hidden])`);
    check(after === '[true,true]', `the row shows the adjusted advice and the editor closes (saw ${after})`);
    clean();
  }

  // Performance budget: a log of ITEMS items (check/budget.mjs). The fold time is the page's own, from the
  // wasm; the first useful paint is the first list row on screen. Numbers go in the log so a budget is set from them.
  await open(`${connected}`, { log: bigLog(ITEMS), pageSize: 500 });
  await page.eval(`(() => {
    window.__firstRow = null;
    const mark = () => { if (!document.querySelector('.row')) return false; window.__firstRow = performance.now(); return true; };
    if (!mark()) new MutationObserver((_, o) => mark() && o.disconnect()).observe(document, { childList: true, subtree: true });
    return true;
  })()`);
  await page.waitFor(`performance.getEntriesByName('kbl-live').length > 0`, 120000);
  const perf = await page.eval(`(() => {
    const live = performance.getEntriesByName('kbl-live')[0];
    const fcp = performance.getEntriesByName('first-contentful-paint')[0];
    return { foldMs: live.detail.foldMs, liveMs: live.startTime, firstRowMs: window.__firstRow, fcpMs: fcp ? fcp.startTime : null, rows: document.querySelectorAll('.row').length };
  })()`);
  console.log(`     perf: ${JSON.stringify(perf)}`);
  check(perf.rows > 0 && perf.firstRowMs !== null, `perf: the ${ITEMS}-item log lists rows (${perf.rows})`);
  check(perf.foldMs <= FOLD_BUDGET_MS.browser, `perf: folding ${ITEMS} items took ${Math.round(perf.foldMs)} ms, budget ${FOLD_BUDGET_MS.browser} ms`);
  check(perf.firstRowMs !== null && perf.firstRowMs <= PAINT_BUDGET_MS.firstUseful, `perf: first useful paint (first row) at ${Math.round(perf.firstRowMs)} ms, budget ${PAINT_BUDGET_MS.firstUseful} ms`);
  check(perf.liveMs <= PAINT_BUDGET_MS.fullLoad, `perf: live with the whole log at ${Math.round(perf.liveMs)} ms, budget ${PAINT_BUDGET_MS.fullLoad} ms`);
  clean();
} catch (e) {
  console.error('page problems:', page.problems);
  console.error('status:', await status().catch(() => '?'), '| body:', await page.eval(`document.body.innerText.slice(0, 300)`).catch(() => '?'));
  throw e;
} finally {
  await page.close();
  await server.close();
}
if (failures.length) {
  console.error(`${failures.length} check(s) failed`);
  process.exit(1);
}
