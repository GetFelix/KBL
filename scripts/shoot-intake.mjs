// Takes the intake frame's stills with the repo's own headless Chrome launcher.
// Usage: node scripts/shoot-intake.mjs <out-dir>
import { mkdirSync, writeFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { join } from 'node:path';
import { launch, serve } from './lib/browser.mjs';

const out = process.argv[2];
if (!out) { console.error('usage: shoot-intake.mjs <out-dir>'); process.exit(2); }
mkdirSync(out, { recursive: true });
const SHOTS = [
  ['dashboard-queue', 'fixture-work.json', 'view=queue&select=gh-789', 1280, 820, 'dark'],
  ['dashboard-board', 'fixture-work.json', 'view=board&select=gh-803', 1280, 820, 'dark'],
  ['dashboard-capped', 'fixture-work-capped.json', 'view=board', 1280, 820, 'dark'],
  ['dashboard-metrics', 'fixture-work.json', 'view=metrics', 1280, 1100, 'dark'],
  ['dashboard-light', 'fixture-work.json', 'view=queue&select=jira-214', 1280, 820, 'light'],
  ['dashboard-burst', 'fixture-work-burst.json', 'view=board', 1280, 820, 'dark'],
  ['dashboard-phone', 'fixture-work.json', 'view=queue&select=gh-790', 390, 844, 'dark'],
  ['dashboard-phone-board', 'fixture-work-capped.json', 'view=board', 390, 844, 'light'],
  ['dashboard-phone-metrics', 'fixture-work.json', 'view=metrics', 390, 844, 'dark'],
];
const server = await serve(fileURLToPath(new URL('..', import.meta.url)));
const page = await launch();
try {
  for (const [name, fixture, extra, w, h, theme] of SHOTS) {
    await page.setViewport(w, h, 2);
    await page.goto(`${server.origin}/prototype/intake-frame.html?fixture=${fixture}&theme=${theme}&${extra}`);
    await page.waitFor(`document.documentElement.dataset.ready === '1'`);
    await page.eval(`document.fonts.ready.then(() => true)`);
    await new Promise((ok) => setTimeout(ok, 300));
    const shot = await page.send('Page.captureScreenshot', { format: 'png' });
    writeFileSync(join(out, `${name}.png`), Buffer.from(shot.data, 'base64'));
    console.log('wrote', name);
  }
} finally {
  await page.close();
  await server.close();
}
