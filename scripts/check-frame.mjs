// Loads the style frame in headless Chrome and checks that both fixtures play
// without errors and that the frame shows every element the design lists.
// Usage: node scripts/check-frame.mjs   (set CHROME if Chrome is not on the usual path)
import { fileURLToPath } from 'node:url';
import { launch, serve } from './lib/browser.mjs';

// One row per element in the design's table ("What each element is drawn from").
// `kinds`: records that draw it; the row is covered if the run has any of them.
// `flights`: for rows that move, the packet classes the page derives from the run
// and draws (see rawFlights in the page); at least one must be present.
// `lapse` is not a record: the page derives it when a claim passes its deadline.
const ELEMENTS = [
  ['a worker appearing', ['agent_up'], []],
  ['a worker going dark', ['agent_gone', 'lapse'], []],
  ['a task packet, planner to queue', ['task_posted'], ['post']],
  ['queue depth in the hub', ['task_posted', 'task_claimed'], []],
  ['a task packet, queue to worker', ['task_claimed'], ['claim']],
  ['the claim timer around a node', ['task_claimed', 'task_extended'], []],
  ['a task travelling back to the queue', ['lapse', 'task_released'], ['back']],
  ['a result packet', ['task_done'], ['result']],
  ['a result that fades halfway', ['task_duplicate'], ['dup']],
  ['a task dropping out of the hub', ['task_dead'], ['dead']],
  ['a board card and its version', ['board_put'], ['write']],
  ['a write bouncing off a card', ['board_conflict'], ['conflict']],
  ['a lock taken on a board card', ['lease_taken'], []],
  ['a lock renewed', ['lease_renewed'], []],
  ['a lock released', ['lease_released'], []],
  ['a lock lost', ['lease_lost'], []],
  ['a chord between two workers', ['message'], ['msg']],
  ['a pulse on a node while it thinks', ['model_started', 'model_call'], []],
  ['the budget ring', ['run_started', 'model_call'], []],
  ['a red bounce off whatever was targeted', ['refused'], ['refused']],
  ['the end of the run', ['run_ended'], []],
];
// The burst is a load fixture: it only has to cover the work and recovery rows.
const BURST_ROWS = new Set(['a worker appearing', 'a worker going dark', 'a task packet, planner to queue', 'a task packet, queue to worker', 'a task travelling back to the queue', 'a result packet', 'the end of the run']);
const FIXTURES = [
  ['fixture-run.json', ELEMENTS],
  ['fixture-burst.json', ELEMENTS.filter(([name]) => BURST_ROWS.has(name))],
];
const VIEWPORTS = [['laptop', 1280, 800], ['phone', 390, 844]];

const failures = [];
const fail = (msg) => { failures.push(msg); console.error('FAIL', msg); };

const server = await serve(fileURLToPath(new URL('..', import.meta.url)));
const page = await launch();
try {
  for (const [fixture, elements] of FIXTURES) {
    for (const [name, w, h] of VIEWPORTS) {
      const label = `${fixture} on ${name}`;
      await page.setViewport(w, h);
      await page.goto(`${server.origin}/prototype/style-frame.html?fixture=${fixture}&t=1`);
      try {
        await page.waitFor(`document.documentElement.dataset.ready === '1'`);
      } catch {
        fail(`${label}: the page did not become ready${page.problems.length ? `: ${page.problems.join('; ')}` : ''}`);
        continue;
      }
      const info = await page.eval('window.frameInfo()');
      for (const [element, kinds, flights] of elements) {
        if (!kinds.some((k) => info.kinds.includes(k))) fail(`${label}: no record draws "${element}" (needs one of ${kinds.join(', ')})`);
        if (flights.length && !flights.some((c) => info.flights.includes(c))) fail(`${label}: "${element}" never flies (needs a ${flights.join(' or ')} packet)`);
      }
      // The scene shares one canvas with the board and hub, so a frame is only
      // alive if it differs from the empty start of the run.
      const pixels = (T) => `(() => {
        window.renderAt(${T}, 8);
        const c = document.getElementById('scene');
        return c.getContext('2d').getImageData(0, 0, c.width, c.height).data;
      })()`;
      await page.eval(`(window.__start = ${pixels(0)}, true)`);
      for (const f of [0.25, 0.5, 0.75]) {
        const changed = await page.eval(`(() => {
          const d = ${pixels(`${f} * window.runEnd`)};
          let n = 0;
          for (let i = 0; i < d.length; i += 4) if (Math.abs(d[i] - __start[i]) + Math.abs(d[i + 1] - __start[i + 1]) + Math.abs(d[i + 2] - __start[i + 2]) > 24) n++;
          return n / (d.length / 4);
        })()`);
        if (changed < 0.005) fail(`${label}: the frame at ${f * 100}% of the run looks the same as the start`);
      }
      for (const p of page.problems) fail(`${label}: ${p}`);
      console.log(`ok   ${label}: ${info.kinds.length} record kinds, flights ${info.flights.join(' ')}`);
    }
  }
} finally {
  await page.close();
  await server.close();
}
if (failures.length) { console.error(`${failures.length} check(s) failed`); process.exit(1); }
