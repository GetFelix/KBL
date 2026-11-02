// Loads the style frame in headless Chrome and checks that both fixtures play
// without errors and that the frame shows every element the design lists.
// Usage: node scripts/check-frame.mjs   (set CHROME if Chrome is not on the usual path)
import { launch, serve } from './lib/browser.mjs';

// One row per element in the design's table ("What each element is drawn from").
// Each lists the records that draw it; the element is covered if any is in the run.
// `lapse` is not a record: the page derives it when a claim passes its deadline.
const ELEMENTS = [
  ['a worker appearing', ['agent_up']],
  ['a worker going dark', ['agent_gone', 'lapse']],
  ['a task packet, planner to queue', ['task_posted']],
  ['queue depth in the hub', ['task_posted']],
  ['a task packet, queue to worker', ['task_claimed']],
  ['the claim timer around a node', ['task_claimed']],
  ['a task travelling back to the queue', ['lapse', 'task_released']],
  ['a result packet', ['task_done']],
  ['a result that fades halfway', ['task_duplicate']],
  ['a task dropping out of the hub', ['task_dead']],
  ['a board card and its version', ['board_put']],
  ['a write bouncing off a card', ['board_conflict']],
  ['a lock on a board card', ['lease_taken']],
  ['a lock renewed, released or lost', ['lease_renewed', 'lease_released', 'lease_lost']],
  ['a chord between two workers', ['message']],
  ['a pulse on a node while it thinks', ['model_started']],
  ['the budget ring', ['run_started']],
  ['a red bounce off whatever was targeted', ['refused']],
];
// The burst is a load fixture: it only has to cover the work and recovery rows.
const BURST_ELEMENTS = ELEMENTS.filter(([, kinds]) => kinds.some((k) => ['agent_up', 'agent_gone', 'task_posted', 'task_claimed', 'task_done', 'task_released', 'run_started'].includes(k)));
const FIXTURES = [
  ['fixture-run.json', ELEMENTS],
  ['fixture-burst.json', BURST_ELEMENTS],
];
const VIEWPORTS = [['laptop', 1280, 800], ['phone', 390, 844]];

const failures = [];
const fail = (msg) => { failures.push(msg); console.error('FAIL', msg); };

const server = await serve(new URL('..', import.meta.url).pathname);
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
        fail(`${label}: the page did not become ready`);
        continue;
      }
      const info = await page.eval('window.frameInfo()');
      for (const [element, kinds] of elements) {
        if (!kinds.some((k) => info.kinds.includes(k))) fail(`${label}: nothing draws "${element}" (needs one of ${kinds.join(', ')})`);
      }
      // A frame is drawn at several moments; each must have something on it.
      for (const f of [0.02, 0.25, 0.5, 0.75, 1]) {
        const filled = await page.eval(`(() => {
          window.renderAt(${f} * window.runEnd, 8);
          const c = document.getElementById('scene');
          const d = c.getContext('2d').getImageData(0, 0, c.width, c.height).data;
          const bg = [d[0], d[1], d[2]];
          let n = 0;
          for (let i = 0; i < d.length; i += 4) if (Math.abs(d[i] - bg[0]) + Math.abs(d[i + 1] - bg[1]) + Math.abs(d[i + 2] - bg[2]) > 24) n++;
          return n / (d.length / 4);
        })()`);
        if (filled < 0.01) fail(`${label}: the frame at ${f * 100}% of the run is blank`);
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
