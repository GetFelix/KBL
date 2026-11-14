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
  check(await page.eval(`[...document.querySelectorAll('.panel dt')].map((d) => d.textContent).join(',').startsWith('Stage')`), 'the panel shows the recorded fields');
  await shoot('board-panel-dark');
  await press(page, 'Escape');
  check(await page.eval(`document.activeElement?.dataset.id === ${JSON.stringify(card)}`), 'closing the panel gives focus back to the card');
  await press(page, 'End');
  const lastInColumn = await page.eval(`(() => { const c = document.activeElement.closest('.column'); return document.activeElement === [...c.querySelectorAll('.card')].at(-1); })()`);
  check(lastInColumn && (await page.eval(colOf)) === 2, 'End goes to the last card of the column, not of the board');

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
  check((await announced()) === 'gh-3001 is no longer in this view.', `the card leaving is announced (${await announced()})`);
  await page.eval(`window.__push({ kind: 'claimed', item: 'gh-2219', attempt: 1, deadline_ms: 1900000000000, actor: 'agent', by: 'agent-1', at_ms: 1800000000001 })`);
  await page.waitFor(`!!document.querySelector('#column-held + ul .card[data-id=gh-2219]')`);
  check(await focused('.card[data-id=gh-2219]'), 'a card a record moves to another column keeps the focus');
  check((await announced()) === 'gh-2219 moved to Held.', `the move is announced (${await announced()})`);

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
  const offered = await page.eval(`[...document.querySelectorAll('.filter select')].map((s) => s.dataset.facet).join(' ')`);
  check(offered === 'source tier repo', `filters offer the recorded facets (${offered})`);
  const shownValues = await page.eval(`[...document.querySelectorAll('.filter select')].map((s) => s.selectedOptions[0]?.textContent ?? '').join(',')`);
  check(shownValues === 'Any,Any,Any', `every filter with no value shows Any, not a blank (${shownValues})`);
  await press(page, 'f');
  check(await focused('#filter-source'), 'F focuses the first filter');
  await page.eval(`(() => { const s = document.querySelector('#filter-repo'); s.value = 'GetFelix/felix'; s.dispatchEvent(new Event('change')); return true; })()`);
  await page.waitFor(`location.search.includes('repo=GetFelix')`);
  const repoRows = await page.eval(`document.querySelectorAll('.row').length`);
  check(repoRows === 5, `a repo filter keeps only that repo's items (${repoRows})`);
  check(await page.eval(`document.querySelector('.filter-summary').textContent === 'Showing 5 of 14: Repo GetFelix/felix'`), 'the summary says what the filters hide');
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
  await page.waitFor(`document.querySelectorAll('.row').length === 14 && !location.search.includes('tier=')`);
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
  await page.waitFor(`document.querySelectorAll('.row').length === 14`);
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
  await page.eval(`document.querySelector('.panel .blockers a').focus()`);
  await page.eval(`window.__push({ kind: 'linked', item: 'blk-40', to: 'gh-2204', actor: 'person', by: 'maintainer', at_ms: 1800000000040 })`);
  await page.waitFor(`document.querySelector('.panel .blockers')?.textContent.includes('gh-2204')`);
  check(await page.eval(`document.activeElement?.dataset.focus === 'blocker:gw-31'`), 'a live redraw of the panel leaves focus on the same blocker link');
  await page.eval(`document.querySelector('.panel .blockers a').click()`);
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
  await page.eval(`window.__push({ kind: 'linked', item: 'blk-43', to: 'blk-42', ${ids(12)} })`);
  await page.waitFor(`!!document.querySelector('.card[data-id=blk-43]')`);
  const column = (id) => page.eval(`document.querySelector('.card[data-id=${id}]')?.closest('.column')?.getAttribute('aria-labelledby') ?? ''`);
  check((await column('blk-40')) === (await column('blk-43')), 'two blocked cards share a column');
  await page.eval(`document.querySelector('.card[data-id=blk-40]').focus()`);
  for (const item of ['gw-31', 'jira-300']) {
    await page.eval(`window.__push({ kind: 'closed', item: '${item}', why: 'merged', ${ids(20)} })`);
  }
  await page.waitFor(`!document.querySelector('.card[data-id=blk-40]')`);
  check(await focused('.card[data-id=blk-43]'), 'an unblocked card hands the focus to its neighbour in the column');
  await page.eval(`window.__push({ kind: 'shipped', item: 'blk-42', release: 'v1', ${ids(30)} })`);
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
