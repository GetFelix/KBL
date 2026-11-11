// Loads the intake frame in headless Chrome at laptop and phone size and checks
// that both fixtures fold into the design's columns, that selecting an item
// draws its dependency chain, and that the page stays readable under the burst
// (docs/work-tracker.md "The intake view").
// Usage: node scripts/check-intake.mjs
import { fileURLToPath } from 'node:url';
import { launch, serve } from './lib/browser.mjs';

const STAGES = ['arrived', 'needs', 'blocked', 'ready', 'held', 'done'];
const VIEWPORTS = [['laptop', 1280, 800], ['phone', 390, 844]];
const failures = [];
const fail = (msg) => { failures.push(msg); console.error('FAIL', msg); };
const check = (ok, msg) => { if (!ok) fail(msg); };

const server = await serve(fileURLToPath(new URL('..', import.meta.url)));
const page = await launch();
const open = async (fixture, query = '') => {
  await page.goto(`${server.origin}/prototype/intake-frame.html?fixture=${fixture}&${query}`);
  await page.waitFor(`document.documentElement.dataset.ready === '1'`);
};
try {
  for (const [size, w, h] of VIEWPORTS) {
    await page.setViewport(w, h);

    // The hand-written morning covers every column, source, tier and a blocked chain.
    let label = `${size} fixture-work.json`;
    await open('fixture-work.json');
    check(page.problems.length === 0, `${label}: page errors: ${page.problems.join('; ')}`);
    let info = await page.eval('window.intakeInfo()');
    for (const s of STAGES) check(info.colInfo[s].count > 0, `${label}: the ${s} column is empty`);
    check(info.sources.length === 3, `${label}: expected GitHub, Azure DevOps and Jira, got ${info.sources}`);
    check([0, 1, 2, 3].every((t) => info.tiers.includes(t)), `${label}: a tier from p0 to p3 is missing`);
    check(info.colInfo.held.count === (await page.eval(`document.querySelectorAll('#c-held .agent').length`)), `${label}: a held item does not name its agent`);
    check(info.colInfo.arrived.oldest > 0 && info.colInfo.ready.oldestRef, `${label}: columns show no oldest age`);
    check(await page.eval(`document.getElementById('rates').textContent.includes('/day')`), `${label}: no arrival and completion rates in the footer`);
    const noScroll = await page.eval(`document.documentElement.scrollWidth <= innerWidth && (innerWidth > 760 || document.getElementById('main').scrollWidth <= document.getElementById('main').clientWidth)`);
    check(noScroll, `${label}: the page scrolls sideways`);

    // Selecting the end of the chain finds everything it waits on and blocks.
    await page.eval(`window.select('gh-801')`);
    info = await page.eval('window.intakeInfo()');
    for (const k of ['gh-798', 'jira-218']) check(info.chain.includes(k), `${label}: the chain of gh-801 misses ${k}`);
    check(await page.eval(`document.querySelectorAll('.row.chain').length`) === 2, `${label}: chain rows are not marked`);
    check(await page.eval(`document.getElementById('dock').classList.contains('open')`), `${label}: no detail for the selected item`);
    await new Promise((ok) => setTimeout(ok, 150));
    info = await page.eval('window.intakeInfo()');
    if (size === 'laptop') check(info.links === 2, `${label}: expected 2 dependency lines, drawn ${info.links}`);
    await page.eval(`window.select(null)`);

    // Scrubbing back folds fewer events.
    await page.eval(`window.scrubTo(+document.getElementById('scrub').min - 1)`);
    check((await page.eval('window.intakeInfo()')).open === 0, `${label}: scrubbed to the start still shows open items`);

    // The burst: hundreds of items stay a screenful of capped columns.
    label = `${size} fixture-work-burst.json`;
    await open('fixture-work-burst.json');
    check(page.problems.length === 0, `${label}: page errors: ${page.problems.join('; ')}`);
    info = await page.eval('window.intakeInfo()');
    check(info.items >= 300, `${label}: fewer than 300 items`);
    const rows = await page.eval(`document.querySelectorAll('.row').length`);
    check(rows <= 90, `${label}: ${rows} rows drawn; the columns should cap`);
    const clipped = await page.eval(`[...document.querySelectorAll('.row .l1')].filter((e) => e.scrollWidth > e.clientWidth + 1).map((e) => e.textContent).length`);
    check(clipped === 0, `${label}: ${clipped} rows overflow their first line`);
    const total = STAGES.reduce((n, s) => n + info.colInfo[s].count, 0);
    check(total >= 300, `${label}: columns hold ${total} items, expected the whole burst`);
    await page.eval(`window.select(document.querySelector('#c-blocked .row').dataset.k)`);
    info = await page.eval('window.intakeInfo()');
    check(info.chain.length > 1, `${label}: a blocked item has no chain`);
  }
} finally {
  await page.close();
  await server.close();
}
if (failures.length) process.exit(1);
console.log('intake ok');
