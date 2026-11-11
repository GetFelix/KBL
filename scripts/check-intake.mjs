// Loads the intake frame in headless Chrome at laptop and phone size and checks
// that both fixtures fold into the design's columns, that selecting an item
// draws its dependency chain, and that the page stays readable under the burst
// (docs/work-tracker.md "The dashboard").
// Usage: node scripts/check-intake.mjs
import { fileURLToPath } from 'node:url';
import { launch, serve } from './lib/browser.mjs';

const STAGES = ['triage', 'needs', 'blocked', 'ready', 'held', 'review', 'done'];
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
    await open('fixture-work.json', 'view=board');
    check(page.problems.length === 0, `${label}: page errors: ${page.problems.join('; ')}`);
    let info = await page.eval('window.intakeInfo()');
    for (const s of STAGES) check(info.colInfo[s].count > 0, `${label}: the ${s} column is empty`);
    check(await page.eval(`!document.getElementById('c-sources')`), `${label}: the Sources column is back; sources belong in the header chip`);
    check(await page.eval(`document.getElementById('srcbtn').textContent.includes('/24h')`), `${label}: no arrivals in the sources chip`);
    check(info.sources.length === 3, `${label}: expected GitHub, Azure DevOps and Jira, got ${info.sources}`);
    check([0, 1, 2, 3].every((t) => info.tiers.includes(t)), `${label}: a tier from p0 to p3 is missing`);
    check(info.colInfo.held.count === (await page.eval(`document.querySelectorAll('#c-held .agent').length`)), `${label}: a held item does not name its agent`);
    check(info.colInfo.triage.oldest > 0 && info.colInfo.ready.oldestRef, `${label}: columns show no oldest age`);
    check(await page.eval(`document.getElementById('rates').textContent.includes('/day')`), `${label}: no arrival and completion rates in the footer`);
    check(/Binding stage, \w/.test(await page.eval(`document.getElementById('bind').textContent`)), `${label}: the footer does not name a binding stage`);
    check(await page.eval(`document.querySelectorAll('#c-review .row[data-stage="review"]').length === ${info.colInfo.review.count}`), `${label}: In review rows do not match the count`);
    check(await page.eval(`document.querySelector('#c-review .row').textContent.includes('CI')`) && await page.eval(`/\\+\\d+ .\\d+ · \\d+ files? · CI/.test(document.querySelector('#c-review .row').textContent)`), `${label}: an In review card does not show size and CI`);
    check(await page.eval(`document.querySelectorAll('#c-blocked .root').length > 0 && document.querySelector('#c-blocked .root').textContent.includes('holds up')`), `${label}: Blocked shows no root blocker with "holds up N"`);
    check(await page.eval(`document.querySelectorAll('#c-triage .row').length > 0 && [...document.querySelectorAll('#c-triage .row')].some((r) => r.textContent.includes('why P'))`), `${label}: triage cards show no proposal reason`);
    check(await page.eval(`document.querySelectorAll('#c-needs .tierhead').length >= 2`), `${label}: Needs you does not group failures by cause`);
    check(await page.eval(`getComputedStyle(document.querySelector('.age.hot, .age.warn')).color !== getComputedStyle(document.querySelector('.ref')).color`), `${label}: no age is coloured against the stage percentiles`);
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
    if (size === 'laptop') {
      check(info.links === 2, `${label}: expected 2 dependency lines, drawn ${info.links}`);
      check(info.crossings === 0, `${label}: ${info.crossings} chain segments cross a card that is not in the chain`);
    }
    await page.eval(`window.select('gh-803')`);
    await new Promise((ok) => setTimeout(ok, 150));
    info = await page.eval('window.intakeInfo()');
    if (size === 'laptop') check(info.links === 2 && info.crossings === 0, `${label}: the chain of gh-803 crosses a card (${info.crossings}) or misses a line (${info.links})`);
    await page.eval(`window.select(null)`);

    // Scrubbing back folds fewer events.
    await page.eval(`window.scrubTo(+document.getElementById('scrub').min - 1)`);
    check((await page.eval('window.intakeInfo()')).open === 0, `${label}: scrubbed to the start still shows open items`);


    // My queue is the landing view; its count is what waits on the viewer.
    label = `${size} my queue`;
    await open('fixture-work.json');
    info = await page.eval('window.intakeInfo()');
    check(info.view === 'queue', `${label}: the page does not land on My queue`);
    // From fixture-work.json: 2 questions and 2 failures (Needs you), 6 proposals (Triage minus
    // the one the agent has not read), and 2 PRs waiting on the viewer.
    const waiting = info.colInfo.needs.count + (info.colInfo.triage.count - 1) + 2;
    check(info.queue.count === waiting, `${label}: expected ${waiting} waiting on the viewer, got ${info.queue.count} ${JSON.stringify(info.queue.kinds)}`);
    check(info.queue.rows === info.queue.count, `${label}: ${info.queue.rows} rows for a count of ${info.queue.count}`);
    check(info.title.startsWith(`(${info.queue.count})`), `${label}: the tab title does not carry the count: ${info.title}`);
    const ages = await page.eval(`[...document.querySelectorAll('#qlist .qrow .age')].map((e) => e.textContent)`);
    check(ages.length > 2 && ages[0] !== ages[ages.length - 1], `${label}: queue rows do not carry ages`);
    check(await page.eval(`document.getElementById('legend').textContent.includes('move')`) || size === 'phone', `${label}: no key legend`);
    check(await page.eval(`document.getElementById('headline').textContent.includes('waiting on you')`), `${label}: no headline sentence`);

    // Keyboard: j selects, a accepts a triage proposal, and the record lands in the event list.
    const press = (k) => page.eval(`document.dispatchEvent(new KeyboardEvent('keydown', { key: '${k}', bubbles: true }))`);
    for (let i = 0; i < 6; i++) { await press('j'); const k = await page.eval(`document.querySelector('.qrow.sel')?.dataset.kind`); if (k === 'triage') break; }
    check(await page.eval(`document.querySelector('.qrow.sel')?.dataset.kind`) === 'triage', `${label}: j did not reach a triage card`);
    const key = await page.eval(`document.querySelector('.qrow.sel').dataset.k`);
    const before = info.queue.count;
    await press('a');
    info = await page.eval('window.intakeInfo()');
    check(info.queue.count === before - 1, `${label}: accepting did not shrink the queue (${before} to ${info.queue.count})`);
    check(info.localEvents.includes('triaged'), `${label}: accepting wrote no triaged record`);
    check(await page.eval(`document.querySelectorAll('#evlist li').length`) >= 1 && await page.eval(`document.getElementById('evlist').textContent.includes('${key}')`), `${label}: the event list does not show the record`);
    check(await page.eval(`document.getElementById('qn').textContent`) === String(before - 1), `${label}: the tab count is stale`);
    // The accepted item left Triage on the board, so the fold story holds.
    await page.eval(`window.setView('board')`);
    check(await page.eval(`!document.querySelector('#c-triage .row[data-k="${key}"]')`), `${label}: the accepted item is still in Triage`);
    check(['ready', 'blocked'].includes(await page.eval(`window.stageOf('${key}')`)), `${label}: the accepted item is not Ready or Blocked`);
    // A failure can be retried, taken, and the review can be approved; each appends a record.
    await page.eval(`window.select('gh-774')`); await press('r');
    await page.eval(`window.select('gh-789')`); await press('a');
    await page.eval(`window.select('gh-812')`); await press('d');
    info = await page.eval('window.intakeInfo()');
    for (const k of ['retried', 'approved', 'rejected']) check(info.localEvents.includes(k), `${label}: no ${k} record after the action`);
    check(info.queue.count === before - 1 - 1 - 1 + (await page.eval(`window.intakeInfo().queue.kinds.merge ?? 0`)), `${label}: the queue count after three more actions is ${info.queue.count}`);
    await page.eval(`window.setView('queue')`);
    // Scrubbed back, actions are off.
    await page.eval(`window.scrubTo(+document.getElementById('scrub').min + 3600)`);
    check(await page.eval(`window.act('a')`) === false, `${label}: an action ran while scrubbed back`);

    // The review limit: reached in one fixture, with agents paused in the header.
    label = `${size} fixture-work-capped.json`;
    await open('fixture-work-capped.json', 'view=board');
    info = await page.eval('window.intakeInfo()');
    check(info.review.full && info.review.count === 6, `${label}: expected 6/6 in review, got ${info.review.count}/${info.review.cap}`);
    check(await page.eval(`document.getElementById('capchip').textContent`) === 'review full: agents paused, 6/6', `${label}: the header does not say review is full`);
    check(info.binding === 'review', `${label}: the binding stage is ${info.binding}, expected review`);
    check(await page.eval(`document.querySelector('#c-ready .banner')?.textContent.includes('paused')`), `${label}: the Ready column does not say agents are paused`);
    await open('fixture-work.json', 'view=board');
    check(await page.eval(`document.getElementById('capchip').textContent.includes('5/6')`), `${label}: the base fixture should show 5/6`);

    // In review sub-states and the handoff.
    await open('fixture-work-capped.json', 'view=board');
    const sub = await page.eval(`document.getElementById('c-review').textContent`);
    for (const t of ['CI green, no reviewer', 'approved, not merged', 'CI red']) check(sub.includes(t), `${label}: In review does not show "${t}"`);
    await page.eval(`window.select('gh-771')`);
    check(await page.eval(`!!document.querySelector('#dock [data-act="v"]')`), `${label}: no request review action on an unassigned PR`);
    check(await page.eval(`document.getElementById('dock').textContent.includes("handoff")`), `${label}: the PR card has no agent handoff`);
    await page.eval(`window.select('ado-4383')`);
    check(await page.eval(`!!document.querySelector('#dock [data-act="m"]')`), `${label}: no merge action on an approved PR`);

    // Metrics: every chart is present, and nothing is per person.
    label = `${size} metrics`;
    await open('fixture-work.json', 'view=metrics');
    check(page.problems.length === 0, `${label}: page errors: ${page.problems.join('; ')}`);
    const m = await page.eval(`document.getElementById('v-metrics').textContent`);
    for (const t of ['Waiting on a human', 'First human action', 'Cost and effort per merged fix', 'Time in each stage', 'Throughput against arrivals', 'Cumulative flow', 'Blocked time', 'Rework', 'by task class', 'merged after human edits', 'reverted in 14d']) check(m.includes(t), `${label}: missing ${t}`);
    check(await page.eval(`document.querySelectorAll('#v-metrics svg').length >= 4`), `${label}: fewer than four charts`);
    check(!/\b(dana|kim|ravi)\b/i.test(m), `${label}: a person is named in the metrics`);
    check(await page.eval(`document.documentElement.scrollWidth <= innerWidth`), `${label}: the page scrolls sideways`);

    // The burst: hundreds of items stay a screenful of capped columns.
    label = `${size} fixture-work-burst.json`;
    await open('fixture-work-burst.json', 'view=board');
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
    await new Promise((ok) => setTimeout(ok, 150));
    info = await page.eval('window.intakeInfo()');
    if (size === 'laptop') check(info.crossings === 0, `${label}: ${info.crossings} chain segments cross a card that is not in the chain`);
  }
} finally {
  await page.close();
  await server.close();
}
if (failures.length) process.exit(1);
console.log('intake ok');
