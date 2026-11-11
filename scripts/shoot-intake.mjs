// Takes the intake frame's stills with the repo's own headless Chrome launcher.
// Usage: node scripts/shoot-intake.mjs <out-dir> [name-prefix]
import { mkdirSync, writeFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { join } from 'node:path';
import { launch, serve } from './lib/browser.mjs';

const out = process.argv[2];
if (!out) { console.error('usage: shoot-intake.mjs <out-dir>'); process.exit(2); }
mkdirSync(out, { recursive: true });
const SHOTS = [
  ['dashboard-queue', 'fixture-work.json', 'view=queue&select=gh-789', 1280, 820, 'dark'],
  ['dashboard-board', 'fixture-work.json', 'view=board&select=gh-803', 1440, 900, 'dark'],
  ['dashboard-capped', 'fixture-work-capped.json', 'view=board', 1280, 820, 'dark'],
  ['dashboard-metrics', 'fixture-work.json', 'view=metrics', 1280, 1100, 'dark'],
  ['dashboard-light', 'fixture-work.json', 'view=queue&select=jira-214', 1280, 820, 'light'],
  ['dashboard-burst', 'fixture-work-burst.json', 'view=board', 1280, 820, 'dark'],
  ['dashboard-phone', 'fixture-work.json', 'view=queue&select=gh-790', 390, 844, 'dark'],
  ['dashboard-phone-board', 'fixture-work-capped.json', 'view=board', 390, 844, 'light'],
  ['dashboard-phone-metrics', 'fixture-work.json', 'view=metrics', 390, 844, 'dark'],
  ['replay-story', 'fixture-work-replay.json', 'story=gh-792&released=0&autoclaim=0', 1280, 1480, 'dark'],
  ['replay-scrub', 'fixture-work-replay.json', 'story=gh-792&d=3&autoclaim=0', 1280, 1000, 'dark'],
  ['replay-redo', 'fixture-work-replay.json', 'story=gh-792&d=3&redo=1&autoclaim=0&note=Fix%20it%20where%20the%20client%20picks%20the%20offset%2C%20not%20in%20the%20broker.', 1280, 900, 'dark'],
  ['replay-side-by-side', 'fixture-work-replay.json', 'story=gh-792&tab=side&autoclaim=0', 1280, 1000, 'dark'],
  ['replay-limit', 'fixture-work-replay.json', 'story=gh-795&released=0&autoclaim=0', 1280, 1000, 'dark'],
  ['replay-queue-light', 'fixture-work-replay.json', 'view=queue&select=gh-792&autoclaim=0', 1280, 900, 'light'],
  ['replay-phone', 'fixture-work-replay.json', 'story=gh-792&d=3&autoclaim=0', 390, 1700, 'dark'],
];
// The conventions set: every view and state, in both themes, under <out-dir>/ux.
// Each entry is [name, fixture, query, width, height, script to run before the shot].
const UX = [
  ['queue', 'fixture-work.json', 'view=queue&select=ado-4377'],
  ['list', 'fixture-work.json', 'view=list&select=gh-789'],
  ['board', 'fixture-work.json', 'view=board&select=gh-803'],
  ['review-states', 'fixture-work-capped.json', 'view=list&sv=review&stage=review&select=gh-771'],
  ['board-review-full', 'fixture-work-capped.json', 'view=board', `document.body.classList.add('nosb')`],
  ['palette', 'fixture-work.json', 'view=queue&select=gh-774', `window.ux.openPal('all')`],
  ['palette-search', 'fixture-work.json', 'view=board', `(window.ux.openPal('all'), window.ux.type('group'))`],
  ['shortcuts', 'fixture-work.json', 'view=list', `window.ux.openPal('keys')`],
  ['search', 'fixture-work.json', 'view=list&q=group', `document.getElementById('q').focus()`],
  ['filters', 'fixture-work.json', 'view=list&tier=0,1&source=github', `window.ux.openPal('filter')`],
  ['saved-view', 'fixture-work.json', 'view=board&sv=urgent&tier=0,1'],
  ['bulk-undo', 'fixture-work.json', 'view=list&stage=triage', `(window.select('gh-808'), window.act('1'), window.ux.check(['ado-4408', 'gh-809', 'jira-229']))`],
  ['status', 'fixture-work.json', 'view=list&select=gh-774', `window.ux.openPal('status')`],
  ['assign', 'fixture-work.json', 'view=board&select=gh-774', `window.ux.openPal('assign')`],
  ['create', 'fixture-work.json', 'view=list', `(window.ux.openPal('create'), window.ux.type('Publish stalls after a rebalance'))`],
  ['display', 'fixture-work.json', 'view=list&density=compact', `window.ux.openPal('display')`],
  ['compact', 'fixture-work.json', 'view=list&density=compact&select=gh-790'],
  ['empty-queue', 'fixture-work-quiet.json', 'view=queue'],
  ['empty-filter', 'fixture-work.json', 'view=list&q=no%20such%20item'],
  ['loading', 'fixture-work.json', 'loading=1'],
  ['error', 'fixture-work.json', 'view=queue&select=gh-789&fail=1', `window.act('primary')`],
  ['metrics', 'fixture-work.json', 'view=metrics'],
  ['story', 'fixture-work-replay.json', 'story=gh-792&d=3&autoclaim=0'],
  ['phone-queue', 'fixture-work.json', 'view=queue&select=ado-4377', null, 390, 844],
  ['phone-list', 'fixture-work.json', 'view=list', null, 390, 844],
];
for (const theme of ['dark', 'light']) for (const [name, fixture, query, run, w = 1440, h = 900] of UX) SHOTS.push([`ux/${name}-${theme}`, fixture, query, w, h, theme, run, w > 800 ? 1 : 2]);
mkdirSync(join(out, 'ux'), { recursive: true });
const only = process.argv[3];   // optional: shoot only names that start with this
const server = await serve(fileURLToPath(new URL('..', import.meta.url)));
const page = await launch();
try {
  for (const [name, fixture, extra, w, h, theme, run, scale = 2] of SHOTS) {
    if (only && !name.startsWith(only)) continue;
    await page.setViewport(w, h, scale);
    await page.goto(`${server.origin}/prototype/intake-frame.html?fixture=${fixture}&theme=${theme}&${extra}`);
    await page.waitFor(`document.documentElement.dataset.ready === '1'`);
    await page.eval(`document.fonts.ready.then(() => true)`);
    if (run) await page.eval(run);
    await new Promise((ok) => setTimeout(ok, 300));
    const shot = await page.send('Page.captureScreenshot', { format: 'png' });
    writeFileSync(join(out, `${name}.png`), Buffer.from(shot.data, 'base64'));
    console.log('wrote', name);
  }
} finally {
  await page.close();
  await server.close();
}
