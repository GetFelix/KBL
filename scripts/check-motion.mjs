// Checks the style frame's motion rules (docs/design.md "Motion and aggregation"):
// seeking draws the frame playback draws, bursts merge, bookkeeping never moves,
// quiet stretches are skipped, busy edges turn into flow lines, reduced motion
// stops flights, and a frame is cheap enough for 60 per second.
// Usage: node scripts/check-motion.mjs
import { fileURLToPath } from 'node:url';
import { launch, serve } from './lib/browser.mjs';

const FRAME_BUDGET_MS = 20; // p95 of one frame at 16x on the burst; the aim is 16.7 on a laptop, CI runners are slower
const MOVING = new Set(['task_posted', 'task_claimed', 'lapse', 'task_released', 'task_done', 'task_duplicate', 'task_dead', 'board_put', 'board_conflict', 'refused', 'message']);
const NEVER_MERGED = new Set(['refused', 'conflict', 'dead', 'dup']);

const failures = [];
const fail = (msg) => { failures.push(msg); console.error('FAIL', msg); };
const check = (ok, msg) => { if (!ok) fail(msg); };

const server = await serve(fileURLToPath(new URL('..', import.meta.url)));
const page = await launch({ width: 1280, height: 800 });
const open = async (query) => {
  await page.goto(`${server.origin}/prototype/style-frame.html?${query}`);
  await page.waitFor(`typeof window.motionProbe === 'object' && typeof window.renderAt === 'function'`);
};
const wait = (ms) => new Promise((ok) => setTimeout(ok, ms));

try {
  for (const fixture of ['fixture-run.json', 'fixture-burst.json']) {
    const label = fixture;

    // Playback and seeking draw the same frame: take the frame playback just drew,
    // then ask for that moment by seeking.
    await open(`fixture=${fixture}&speed=8`);
    await wait(1500);
    const live = await page.eval(`(() => {
      const state = motionProbe.state();
      const drawn = motionProbe.canvas();
      renderAt(state.T, state.speed, state.wall);
      return { state, same: drawn === motionProbe.canvas() };
    })()`);
    check(live.state.T > 0.5, `${label}: playback did not advance (T=${live.state.T})`);
    check(live.same, `${label}: seeking to T=${live.state.T.toFixed(2)} draws a different frame than playback did`);

    // Nothing carries over from earlier frames: reaching a moment by stepping, by
    // jumping from the end, or from another speed gives the same pixels.
    await open(`fixture=${fixture}&t=1`);
    const history = await page.eval(`(() => {
      const end = window.runEnd, out = [];
      for (const f of [0.3, 0.6, 0.9]) {
        const T = f * end;
        for (let t = 0; t < T; t += 0.5) renderAt(t, 8, 0);
        renderAt(T, 8, 0);
        const stepped = motionProbe.canvas();
        renderAt(end, 8, 0); renderAt(0, 1, 0); renderAt(T, 8, 0);
        const jumped = motionProbe.canvas();
        renderAt(T, 16, 0); renderAt(T, 8, 0);
        const respeed = motionProbe.canvas();
        out.push({ f, ok: stepped === jumped && jumped === respeed });
      }
      return out;
    })()`);
    for (const h of history) check(h.ok, `${label}: the frame at ${h.f * 100}% depends on how it was reached`);

    // Packets merge by edge and window, scaled by speed.
    const info = await page.eval('window.frameInfo()');
    const posted = await page.eval(`(async () => (await (await fetch('${fixture}')).json()).records.filter((r) => r.kind === 'task_posted').length)()`);
    let prev = Infinity;
    for (const speed of [1, 4, 8, 16]) {
      const flights = await page.eval(`motionProbe.merged(${speed})`);
      const win = 0.25 * speed;
      check(flights.every((f) => MOVING.has(f.kind)), `${label} @${speed}x: a bookkeeping record flies (${[...new Set(flights.map((f) => f.kind).filter((k) => !MOVING.has(k)))]})`);
      check(flights.filter((f) => NEVER_MERGED.has(f.cls)).every((f) => f.n === 1), `${label} @${speed}x: a refusal, conflict, dead letter or duplicate was merged`);
      check(flights.filter((f) => f.cls === 'post').reduce((n, f) => n + f.n, 0) === posted, `${label} @${speed}x: merged posts do not add up to ${posted}`);
      const last = new Map();
      for (const f of flights.filter((f) => !NEVER_MERGED.has(f.cls))) {
        const key = `${f.from}|${f.to}|${f.cls}`;
        if (last.has(key)) check(f.t - last.get(key) >= win - 1e-9, `${label} @${speed}x: two ${key} packets ${(f.t - last.get(key)).toFixed(3)} s apart, inside the ${win} s window`);
        last.set(key, f.t);
      }
      check(flights.length <= prev, `${label}: ${flights.length} packets at ${speed}x, more than at a lower speed`);
      prev = flights.length;
    }

    // Quiet stretches: skipped exactly where nothing is in flight or due.
    const quiet = await page.eval(`(() => {
      const bad = [];
      for (const speed of [1, 8]) {
        const spans = motionProbe.quietSpans(speed);
        for (let T = 0; T < window.runEnd; T += 0.25) {
          const q = motionProbe.quiet(T, speed);
          const inSpan = spans.some(([a, b]) => T > a && T < b);
          if (q && motionProbe.inFlight(T, speed) > 0) bad.push('skips while a packet is in flight at ' + T);
          if (q !== inSpan && !spans.some(([a, b]) => Math.abs(T - a) < 1e-6 || Math.abs(T - b) < 1e-6)) bad.push('timeline mark and skip disagree at ' + T + ' @' + speed);
        }
      }
      return bad.slice(0, 3);
    })()`);
    for (const q of quiet) fail(`${label}: ${q}`);
    const spans = await page.eval('motionProbe.quietSpans(1)');
    if (fixture === 'fixture-run.json') {
      check(spans.length > 0, `${label}: no quiet stretch found, so the skip and its timeline mark are untested`);
      check(spans.every(([a, b]) => b > a), `${label}: a quiet span is empty`);
      const inside = await page.eval(`motionProbe.quiet(${spans[0] ? (spans[0][0] + spans[0][1]) / 2 : 0}, 1)`);
      check(inside, `${label}: the middle of a quiet span is not quiet`);
    }
  }

  // The cached backdrop and vignette follow the theme and the layout: a frame
  // drawn from the cache equals one painted with the cache dropped.
  await open('fixture=fixture-run.json&t=1');
  const stale = await page.eval(`(() => {
    const out = [], T = window.runEnd * 0.4;
    const same = (what) => { renderAt(T, 8, 0); const cached = motionProbe.canvas(); motionProbe.dropLayers(); renderAt(T, 8, 0); out.push([what, cached === motionProbe.canvas()]); };
    same('first paint');
    document.getElementById('theme').click(); same('after the theme toggle');
    document.getElementById('theme').click(); same('after toggling back');
    return out;
  })()`);
  for (const [what, ok] of stale) check(ok, `the cached layers are stale ${what}`);
  await page.setViewport(900, 700);
  await wait(200);
  const resized = await page.eval(`(() => { renderAt(window.runEnd * 0.4, 8, 0); const a = motionProbe.canvas(); motionProbe.dropLayers(); renderAt(window.runEnd * 0.4, 8, 0); return a === motionProbe.canvas(); })()`);
  check(resized, 'the cached layers are stale after a resize');
  await page.setViewport(1280, 800);

  // Flow lines: the same rule at a low threshold, so a fixture can exercise it.
  await open('fixture=fixture-burst.json&t=1&flow=20');
  const flow = await page.eval(`(() => {
    let best = 0, at = 0;
    for (let T = 0; T < window.runEnd; T += 0.25) { const n = motionProbe.inFlight(T, 16); if (n > best) { best = n; at = T; } }
    renderAt(at, 16, 0);
    const busy = motionProbe.flowEdges();
    let calm = 0;
    for (let T = 0; T < window.runEnd; T += 0.25) if (motionProbe.inFlight(T, 16) <= 20) { renderAt(T, 16, 0); calm += motionProbe.flowEdges(); }
    return { best, busy, calm, flowAt: motionProbe.flowAt() };
  })()`);
  check(flow.flowAt === 20, 'flow=20 was ignored');
  check(flow.best > 20 && flow.busy > 0, `no flow line drawn with ${flow.best} packets in flight past a threshold of 20`);
  check(flow.calm === 0, 'flow lines drawn while at or under the threshold');
  console.log(`ok   flow lines: ${flow.busy} busy edge(s) at ${flow.best} in flight`);
  await open('fixture=fixture-burst.json&t=1');
  const dflt = await page.eval('motionProbe.flowAt()');
  check(dflt === 150, `default flow threshold is ${dflt}, the design says 150`);

  // Reduced motion: no packet shapes, only flashes along the edge.
  const busyAt = await page.eval(`(() => { let best = 0, at = 0; for (let T = 0; T < window.runEnd; T += 0.25) { const n = motionProbe.inFlight(T, 8); if (n > best) { best = n; at = T; } } return at; })()`);
  await open('fixture=fixture-burst.json&t=1');
  const normal = await page.eval(`(renderAt(${busyAt}, 8, 0), motionProbe.shapes())`);
  await open('fixture=fixture-burst.json&t=1&motion=reduced');
  const reduced = await page.eval(`(renderAt(${busyAt}, 8, 0), { shapes: motionProbe.shapes(), on: motionProbe.reduced() })`);
  check(normal > 0, 'no packets drawn at the busiest moment');
  check(reduced.on && reduced.shapes === 0, `reduced motion still draws ${reduced.shapes} packet shapes`);
  console.log(`ok   reduced motion: ${normal} packets become flashes`);

  // Frame cost on the burst at 16x. headless Chrome rasterises in software and the
  // read-back forces the whole frame, so this is an upper bound for a real GPU canvas.
  for (const dpr of [1, 2]) {
    await page.setViewport(1440, 900, dpr);
    await open('fixture=fixture-burst.json&t=1');
    // Three runs, best p95: a stall on a shared runner should not fail the job. The first pass also warms the JIT and the fonts.
    let ms = null;
    for (let run = 0; run < 3; run++) {
      const got = await page.eval(`(() => {
      const c = document.getElementById('scene').getContext('2d'), end = window.runEnd, t = [];
      for (let i = 0; i < 240; i++) { const a = performance.now(); renderAt((i / 240) * end, 16, 0); c.getImageData(0, 0, 1, 1); t.push(performance.now() - a); }
      t.sort((x, y) => x - y);
      return { p50: t[120], p95: t[228], worst: t[239] };
    })()`);
      if (!ms || got.p95 < ms.p95) ms = got;
    }
    console.log(`${dpr === 1 ? 'ok  ' : 'info'} frame cost at ${dpr}x pixels: p50 ${ms.p50.toFixed(1)} ms, p95 ${ms.p95.toFixed(1)} ms, worst ${ms.worst.toFixed(1)} ms`);
    if (dpr === 1) check(ms.p95 < FRAME_BUDGET_MS, `a burst frame takes ${ms.p95.toFixed(1)} ms at p95, over ${FRAME_BUDGET_MS} ms`);
  }
  for (const p of page.problems) fail(p);
} finally {
  await page.close();
  await server.close();
}
if (failures.length) { console.error(`${failures.length} check(s) failed`); process.exit(1); }
console.log('motion checks ok');
