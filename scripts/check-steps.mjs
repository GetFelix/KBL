// Clicks every kind of element in the style frame and checks that it opens the
// step behind it (docs/design.md "Look", "Clicking").
// Usage: node scripts/check-steps.mjs
import { fileURLToPath } from 'node:url';
import { launch, serve } from './lib/browser.mjs';

// Every source of a click the viewer draws: the element table's shapes plus the log and the captions.
const REQUIRED = ['agent', 'card', 'planner', 'budget', 'tray', 'packet', 'flow', 'log', 'caption', 'marker'];

const failures = [];
const fail = (msg) => { failures.push(msg); console.error('FAIL', msg); };

const server = await serve(fileURLToPath(new URL('..', import.meta.url)));
const page = await launch({ width: 1280, height: 800 });
try {
  const seen = new Map(); // source -> times opened
  for (const [fixture, flow] of [['fixture-run.json', ''], ['fixture-burst.json', '&flow=20']]) {
    await page.goto(`${server.origin}/prototype/style-frame.html?fixture=${fixture}&t=1${flow}`);
    await page.waitFor(`typeof window.motionProbe === 'object'`);
    const end = await page.eval('window.runEnd');
    for (let i = 1; i < 60; i++) {
      const T = (i / 60) * end;
      await page.eval(`renderAt(${T}, ${flow ? 16 : 8}, 0)`);
      const hits = await page.eval('motionProbe.hits()');
      // One click per source per frame is enough; the frames differ.
      const done = new Set();
      for (const h of hits) {
        const src = h.src || h.kind;
        if (done.has(src)) continue;
        done.add(src);
        await page.eval('motionProbe.closePanel()');
        await page.click(h.x, h.y);
        const opened = await page.eval('motionProbe.panelOpen()');
        // The queue has nothing behind it until the first task is posted.
        if (!opened && src === 'tray' && T < (await page.eval('motionProbe.firstPost()'))) continue;
        if (!opened) fail(`${fixture} T=${T.toFixed(1)}: clicking a ${src} at (${h.x.toFixed(0)}, ${h.y.toFixed(0)}) opened nothing`);
        else seen.set(src, (seen.get(src) || 0) + 1);
      }
      // Restore the frame the click may have moved nothing in, then try the timeline markers once.
      if (i === 30) {
        for (const m of (await page.eval('motionProbe.markers()')).slice(0, 6)) {
          await page.eval('motionProbe.closePanel()');
          await page.click(m.x, m.y);
          if (await page.eval('motionProbe.panelOpen()')) seen.set('marker', (seen.get('marker') || 0) + 1);
          else fail(`${fixture}: clicking a timeline marker at (${m.x.toFixed(0)}, ${m.y.toFixed(0)}) opened nothing`);
        }
        await page.eval(`renderAt(${T}, ${flow ? 16 : 8}, 0)`);
      }
    }
  }
  for (const src of REQUIRED) if (!seen.get(src)) fail(`no ${src} was ever on screen to click`);
  console.log(`ok   opened a step from: ${[...seen].map(([k, n]) => `${k} x${n}`).join(', ')}`);
  for (const p of page.problems) fail(p);
} finally {
  await page.close();
  await server.close();
}
if (failures.length) { console.error(`${failures.length} check(s) failed`); process.exit(1); }
