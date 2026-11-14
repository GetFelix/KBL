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
const fakeGateway = (log, mode) => `(() => {
  const log = ${JSON.stringify(log.map((r) => btoa(unescape(encodeURIComponent(JSON.stringify(r))))))};
  const mode = ${JSON.stringify(mode)};
  window.WebSocket = class {
    constructor() { setTimeout(() => this.onopen && this.onopen(), 0); }
    reply(m, ms = 0) { (window.__sent ??= []).push('reply:' + m.type + ':' + (m.records?.length ?? '')); setTimeout(() => { try { window.__sent.push('deliver ' + typeof this.onmessage); this.onmessage && this.onmessage({ data: JSON.stringify(m) }); window.__sent.push('delivered'); } catch (e) { (window.__sent ??= []).push('threw ' + e); } }, ms); }
    send(text) {
      const m = JSON.parse(text);
      (window.__sent ??= []).push(m.type + ':' + (m.from ?? ''));
      if (m.type === 'join') return mode === 'forbidden'
        ? this.reply({ type: 'error', code: 'signed_out', message: 'the ID token expired' })
        : this.reply({ type: 'hello', features: ['read'] });
      if (m.type === 'read') {
        if (mode === 'loading') return;
        const records = log.slice(m.from, m.from + 20).map((payload, i) => ({ offset: m.from + i, payload }));
        return this.reply({ type: 'records', id: m.id, records, next_offset: m.from + records.length });
      }
      if (m.type === 'subscribe') this.reply({ type: 'subscribed', start_offset: m.from, live_offset: m.from });
    }
    close() {}
  };
})();`;

const failures = [];
const check = (ok, what) => {
  console.log(`${ok ? 'ok  ' : 'FAIL'} ${what}`);
  if (!ok) failures.push(what);
};

const KEYS = { Escape: 27, Enter: 13, Tab: 9, ArrowDown: 40, ArrowUp: 38, Home: 36, End: 35 };
async function press(page, key, modifiers = 0) {
  const code = KEYS[key];
  const text = code ? undefined : key;
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
async function open(query, { mode = 'live', theme = 'dark', width = 1280, height = 800 } = {}) {
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

    await press(page, 'k', 2);
    check(await focused('.palette-input'), `${theme}: Ctrl+K opens the palette with focus in its input`);
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
  await press(page, 'k', 2);
  await press(page, 'Escape');
  check(await page.eval(`document.activeElement?.dataset.id === ${JSON.stringify(second)}`), 'closing the palette gives focus back to the row');
  await press(page, 'Enter');
  check(await page.eval(`!document.querySelector('.panel').hidden && location.search.includes('item=')`), 'Enter opens the row in the panel and the address');
  await shoot('panel-dark');
  await press(page, 'Escape');
  check(await page.eval(`document.querySelector('.panel').hidden`), 'Esc closes the panel');

  await press(page, 'k', 2);
  for (const ch of 'shortcuts') await press(page, ch);
  await press(page, 'Enter');
  check(await page.eval(`document.querySelector('dialog.help').open`), 'the palette runs a command by name');
  await press(page, 'Escape');

  await press(page, '/');
  check(await focused('#search'), '/ focuses search');
  for (const ch of 'zzqx') await press(page, ch);
  check(await page.eval(`document.querySelector('.empty h2')?.textContent.startsWith('Nothing matches')`), 'a search that matches nothing says so');
  await axe('no matches');
  await shoot('empty-search-dark');
  await press(page, 'Escape');
  check(await page.eval(`document.querySelectorAll('.row').length > 0`), 'Esc clears the search');

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
  check(await page.eval(`document.querySelector('.list').getAttribute('aria-busy') === 'true'`), 'the first read shows placeholder rows');
  await shoot('loading-dark');

  await open(connected, { mode: 'forbidden' });
  await page.waitFor(`document.querySelector('.status')?.dataset.tone === 'error'`);
  check(await page.eval(`!!document.querySelector('[role=alert] button')`), 'a rejected token stops with a reason and a Reload button');
  await axe('stopped');
  await shoot('error-dark');

  await open(connected, { width: 390, height: 844 });
  await page.waitFor(live);
  await axe('phone');
  await shoot('phone-dark');
} catch (e) {
  console.error('page problems:', page.problems, await page.eval(`JSON.stringify(window.__sent)`).catch(() => '?'));
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
