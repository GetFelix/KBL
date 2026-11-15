// Renders the KBL mark to PNG with the repo's headless Chrome launcher.
// Usage: node scripts/render-mark.mjs
// Writes docs/brand/kbl-mark.png (graphite, for light backgrounds) and
// kbl-mark-dark.png (paper, for dark backgrounds), 512 px with a transparent background.
import { writeFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { launch, serve } from './lib/browser.mjs';

const OUT = [['kbl-mark-light.svg', 'kbl-mark.png'], ['kbl-mark-dark.svg', 'kbl-mark-dark.png']];
const SIZE = 512;
const root = fileURLToPath(new URL('..', import.meta.url));
const server = await serve(root);
const page = await launch({ width: SIZE, height: SIZE });
try {
  await page.send('Emulation.setDefaultBackgroundColorOverride', { color: { r: 0, g: 0, b: 0, a: 0 } });
  for (const [svg, png] of OUT) {
    // The SVGs are drawn on a 64 grid, so a whole-number scale keeps every edge on a pixel.
    await page.setViewport(64, 64, SIZE / 64);
    await page.goto(`${server.origin}/docs/brand/${svg}`);
    const shot = await page.send('Page.captureScreenshot', { format: 'png' });
    writeFileSync(`${root}docs/brand/${png}`, Buffer.from(shot.data, 'base64'));
    console.log('wrote', png);
  }
} finally {
  await page.close();
  await server.close();
}
