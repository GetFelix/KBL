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
  ['intake-chain', 'fixture-work.json', 'select=gh-803', 1280, 820, 'dark'],
  ['intake-morning', 'fixture-work.json', '', 1280, 820, 'dark'],
  ['intake-light', 'fixture-work.json', 'select=gh-801', 1280, 820, 'light'],
  ['intake-burst', 'fixture-work-burst.json', '', 1280, 820, 'dark'],
  ['intake-phone', 'fixture-work.json', '', 390, 844, 'dark'],
  ['intake-phone-chain', 'fixture-work.json', 'select=gh-801', 390, 844, 'light'],
  ['intake-phone-burst', 'fixture-work-burst.json', '', 390, 844, 'dark'],
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
