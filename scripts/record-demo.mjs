// Records a page that owns its own clock as an H.264 mp4 plus a poster webp.
// The page must expose window.demo = { advance(ms), running, errors } (see the
// autoplay demo in prototype/intake-frame.html with &rec=1). The recorder
// advances that clock one frame, screenshots, and repeats, so no frame is
// dropped and the result does not depend on how fast the machine is.
// Usage: node scripts/record-demo.mjs --out <path-without-extension> [--theme dark|light]
//          [--page prototype/intake-frame.html] [--query "demo=1&rec=1"] [--size 1600x900]
//          [--fps 30] [--crf 28] [--poster <seconds>] [--frames <dir>]
// Needs ffmpeg on PATH.
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { execFileSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import { join, dirname, resolve } from 'node:path';
import { launch, serve } from './lib/browser.mjs';

const arg = Object.fromEntries(process.argv.slice(2).reduce((a, v, i, all) => (v.startsWith('--') ? [...a, [v.slice(2), all[i + 1]]] : a), []));
if (!arg.out) { console.error('usage: record-demo.mjs --out <path-without-extension> [--theme dark|light] [--poster seconds]'); process.exit(2); }
const [W, H] = (arg.size ?? '1600x900').split('x').map(Number);
const FPS = Number(arg.fps ?? 30), CRF = String(arg.crf ?? 28);
const SCALE = 1.25;   // the page lays out at 1280 wide and is shot sharper, so type stays readable at card size
const theme = arg.theme ?? 'dark';
const out = resolve(arg.out);
mkdirSync(dirname(out), { recursive: true });
const frames = arg.frames ? resolve(arg.frames) : mkdtempSync(join(dirname(out), '.frames-'));
mkdirSync(frames, { recursive: true });

const server = await serve(fileURLToPath(new URL('..', import.meta.url)));
const page = await launch({ width: W, height: H });
let n = 0;
try {
  await page.setViewport(Math.round(W / SCALE), Math.round(H / SCALE), SCALE);
  await page.goto(`${server.origin}/${arg.page ?? 'prototype/intake-frame.html'}?${arg.query ?? 'demo=1&rec=1'}&theme=${theme}`);
  await page.waitFor(`document.documentElement.dataset.ready === '1'`);
  await page.eval(`document.fonts.ready.then(() => true)`);
  await page.eval(`(window.demo.start(), window.demo.advance(0))`);
  const step = 1000 / FPS;
  while (await page.eval('window.demo.running')) {
    await page.eval(`window.demo.advance(${step})`);
    const shot = await page.send('Page.captureScreenshot', { format: 'png' });
    writeFileSync(join(frames, `f${String(n++).padStart(5, '0')}.png`), Buffer.from(shot.data, 'base64'));
    if (n > FPS * 120) throw new Error('the demo did not finish within two minutes of its own time');
  }
  const errors = await page.eval('window.demo.errors');
  if (errors.length || page.problems.length) throw new Error(`page errors: ${[...errors, ...page.problems].join('; ')}`);
} finally {
  await page.close();
  await server.close();
}
const ff = (args) => execFileSync('ffmpeg', ['-y', '-loglevel', 'error', ...args], { stdio: 'inherit' });
ff(['-framerate', String(FPS), '-i', join(frames, 'f%05d.png'), '-c:v', 'libx264', '-preset', 'slow', '-crf', CRF, '-pix_fmt', 'yuv420p', '-movflags', '+faststart', '-an', `${out}.mp4`]);
if (arg.poster) ff(['-framerate', String(FPS), '-i', join(frames, 'f%05d.png'), '-vf', `select=eq(n\\,${Math.round(Number(arg.poster) * FPS)})`, '-frames:v', '1', '-c:v', 'libwebp', '-quality', '82', `${out}.webp`]);
if (!arg.frames) rmSync(frames, { recursive: true, force: true });
console.log(`${out}.mp4: ${n} frames, ${(n / FPS).toFixed(1)} s`);
