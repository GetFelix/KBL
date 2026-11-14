// Drives the built dashboard (dist/) in headless Chrome against a fake gateway, then
// checks keyboard flows and runs axe-core in both themes. Exits non-zero on a failed
// flow or any serious or critical axe violation.
// Usage: node check/browser.mjs [screenshot-dir]   (after `npm run build`)
import { mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { createRequire } from 'node:module';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { launch, serve } from '../../scripts/lib/browser.mjs';
import { fixtureLog } from './fixture.mjs';

const shots = process.argv[2];
if (shots) mkdirSync(shots, { recursive: true });
const axeSource = readFileSync(createRequire(import.meta.url).resolve('axe-core/axe.min.js'), 'utf8');

// Replaces WebSocket before the page's code runs: answers `join`, pages `read`s of the
// fixture log, and `subscribe`s from the cursor. `mode` picks the gateway's behaviour.
// `window.__push(record)` delivers one more record as a live event. A throw in the page's
// handler is left to surface as a page exception, which fails the check.
const encode = (r) => btoa(unescape(encodeURIComponent(JSON.stringify(r))));
const fakeGateway = (log, mode) => `(() => {
  const encode = ${encode};
  const log = ${JSON.stringify(log.map(encode))};
  const mode = ${JSON.stringify(mode)};
  window.WebSocket = class {
    constructor() {
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
        const records = log.slice(m.from, m.from + 20).map((payload, i) => ({ offset: m.from + i, payload }));
        return this.reply({ type: 'records', id: m.id, records, next_offset: m.from + records.length });
      }
      if (m.type === 'subscribe') {
        this.liveAt = m.from;
        this.reply({ type: 'subscribed', start_offset: m.from, live_offset: m.from });
      }
    }
    close() {}
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
async function open(query, { mode = 'live', theme = 'dark', width = 1280, height = 800, storage = null } = {}) {
  if (seedScript) await page.send('Page.removeScriptToEvaluateOnNewDocument', { identifier: seedScript });
  seedScript = storage ? (await page.send('Page.addScriptToEvaluateOnNewDocument', { source: seed(storage) })).identifier : null;
  clean();
  loaded = `page ${mode} ${theme} ${width}px`;
  if (script) await page.send('Page.removeScriptToEvaluateOnNewDocument', { identifier: script });
  script = (await page.send('Page.addScriptToEvaluateOnNewDocument', { source: fakeGateway(fixtureLog(), mode) })).identifier;
  await page.setViewport(width, height, 1);
  await page.goto(`${server.origin}/index.html?theme=${theme}&${query}`);
  await page.eval(`document.fonts.ready.then(() => true)`);
}
const connected = 'room=kbl&token=t&gateway=ws://fake';
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
    .then((r) => r.violations.map((v) => ({ id: v.id, impact: v.impact, nodes: v.nodes.length, help: v.help })))`);
  const bad = result.filter((v) => v.impact === 'serious' || v.impact === 'critical');
  for (const v of result) console.log(`     axe ${name}: ${v.impact} ${v.id} (${v.nodes}) ${v.help}`);
  check(bad.length === 0, `axe ${name}: no serious or critical violations (${result.length} total)`);
}

try {
  for (const theme of ['dark', 'light']) {
    await open(connected, { theme });
    await page.waitFor(live);
    const line = await status();
    check(/^. ?Live, \d+ items$/.test(line), `${theme}: the fixture folds with nothing refused (${line})`);
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
  await page.waitFor(`location.search.includes('view=board')`);
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
  check(await page.eval(`[...document.querySelectorAll('.panel dt')].map((d) => d.textContent).join(',').startsWith('Stage')`), 'the panel shows the recorded fields');
  await shoot('board-panel-dark');
  await press(page, 'Escape');
  check(await page.eval(`document.activeElement?.dataset.id === ${JSON.stringify(card)}`), 'closing the panel gives focus back to the card');
  for (const theme of ['dark', 'light']) for (const density of ['comfortable', 'compact']) {
    await open(`${connected}&view=board`, { theme, storage: { 'kbl.dashboard.density': density } });
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
  const offered = await page.eval(`[...document.querySelectorAll('.filter select')].map((s) => s.dataset.facet).join(' ')`);
  check(offered === 'source tier repo', `filters offer the recorded facets (${offered})`);
  await press(page, 'f');
  check(await focused('#filter-source'), 'F focuses the first filter');
  await page.eval(`(() => { const s = document.querySelector('#filter-repo'); s.value = 'GetFelix/felix'; s.dispatchEvent(new Event('change')); return true; })()`);
  await page.waitFor(`location.search.includes('repo=GetFelix')`);
  const repoRows = await page.eval(`document.querySelectorAll('.row').length`);
  check(repoRows === 5, `a repo filter keeps only that repo's items (${repoRows})`);
  check(await page.eval(`document.querySelector('.filter-summary').textContent === 'Showing 5 of 9: Repo GetFelix/felix'`), 'the summary says what the filters hide');
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
  await page.waitFor(`document.querySelectorAll('.row').length === 9 && !location.search.includes('tier=')`);
  check(true, 'Clear filters shows everything and clears the address');

  // Saved views: from the address, to the sidebar, rename, delete, and back from storage.
  await open(`${connected}&view=board&source=github&tier=1`);
  await page.waitFor(live);
  check((await page.eval(`document.querySelectorAll('.card').length`)) === 4, 'a link with a view and filters opens that view');
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
  check(saved === '[{"name":"Source github, Tier 1, board","search":"view=board&source=github&tier=1"}]', 'the view is stored for this viewer');
  await shoot('saved-view-dark');
  await page.eval(`document.querySelector('.saved-row button').click(); true`);
  await page.eval(`(() => { const i = document.querySelector('#view-name'); i.value = 'Urgent GitHub'; return true; })()`);
  await press(page, 'Enter');
  await page.waitFor(`document.querySelector('.saved-row a')?.textContent === 'Urgent GitHub'`);
  check(true, 'Edit renames a view');
  await page.eval(`document.querySelector('.nav a').click(); true`);
  await page.waitFor(`document.querySelectorAll('.row').length === 9`);
  check(await page.eval(`document.querySelector('.nav a').getAttribute('aria-current') === 'page' && !document.querySelector('.saved-row a').hasAttribute('aria-current')`), 'All work resets the view');
  await page.eval(`document.querySelector('.saved-row a').click(); true`);
  await page.waitFor(`document.querySelectorAll('.card').length === 4`);
  check(true, 'a saved view opens from the sidebar');
  await page.eval(`document.querySelector('.saved-row button').click(); true`);
  await page.eval(`[...document.querySelectorAll('.view-dialog button')].find((b) => b.textContent === 'Delete view').click(); true`);
  await page.waitFor(`!document.querySelector('.saved-row a')`);
  check((await page.eval(`localStorage.getItem('kbl.dashboard.views')`)) === '[]', 'Delete removes the view from the sidebar and storage');
  await open(connected, { storage: { 'kbl.dashboard.views': '[{"name":"Tier 2","search":"tier=2"}]' } });
  await page.waitFor(live);
  check((await page.eval(`document.querySelector('.saved-row a')?.textContent`)) === 'Tier 2', 'saved views come back on the next visit');
  await open(connected, { storage: { 'kbl.dashboard.views': 'not json' } });
  await page.waitFor(live);
  check(await page.eval(`!!document.querySelector('.nav-empty')`), 'unreadable storage leaves the sidebar empty, not broken');

  // A fresh page: after a blur, Chrome starts Tab from where focus was, not the top.
  await open(connected);
  await page.waitFor(live);
  await press(page, 'Tab');
  check(await focused('.skip'), 'the first Tab reaches the skip link');

  await open('');
  check(await page.eval(`document.querySelector('.empty h2')?.textContent === 'Connect to a gateway'`), 'without a token the page says how to connect');
  await axe('setup');
  await shoot('setup-dark');

  await open(connected, { mode: 'loading' });
  check(await page.eval(`document.querySelector('.list').hasAttribute('aria-busy')`), 'the first read shows placeholder rows');
  await page.waitFor(`document.querySelector('.status')?.textContent.includes('Reading history')`);
  check(true, 'the status says it is reading history');
  await shoot('loading-dark');

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
  check(
    (await focused('.panel')) && (await page.eval(`(() => { const r = document.querySelector('.panel').getBoundingClientRect(); return r.top >= 0 && r.top < innerHeight; })()`)),
    'on a phone, opening an item moves focus to the panel and scrolls it into view',
  );
  await press(page, 'Escape');
  check(await page.eval(`getComputedStyle(document.querySelector('.menu-button')).display !== 'none' && getComputedStyle(document.querySelector('#sidebar')).display === 'none'`), 'on a phone the sidebar is behind a Menu button');
  await page.eval(`document.querySelector('.menu-button').focus(); true`);
  await press(page, 'Enter');
  check(await page.eval(`document.querySelector('.menu-button').getAttribute('aria-expanded') === 'true' && document.querySelector('#sidebar').contains(document.activeElement)`), 'Menu opens the sidebar and moves focus into it');
  check(await page.eval(`[...document.querySelectorAll('#sidebar button')].some((b) => b.textContent.startsWith('Shortcuts'))`), 'Shortcuts and Commands are reachable on a phone (WCAG 3.2.6)');
  await axe('phone menu');
  await shoot('phone-menu-dark');
  await press(page, 'Escape');
  check(await focused('.menu-button') && (await page.eval(`document.querySelector('.menu-button').getAttribute('aria-expanded') === 'false'`)), 'Esc closes the menu and gives focus back to the button');
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
