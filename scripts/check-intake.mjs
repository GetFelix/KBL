// Loads the intake frame in headless Chrome at laptop and phone size and checks
// that both fixtures fold into the design's columns, that selecting an item
// draws its dependency chain, and that the page stays readable under the burst
// (docs/work-tracker.md "The dashboard"), and that the decision replay explains a
// step from what was recorded up to it and nothing after (docs/decision-replay.md).
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
    check(await page.eval(`document.getElementById('panel').classList.contains('open')`), `${label}: no detail for the selected item`);
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

    // Keyboard: j selects, 1 accepts a triage proposal, and the record lands in the event list.
    const press = (k) => page.eval(`document.dispatchEvent(new KeyboardEvent('keydown', { key: '${k}', bubbles: true }))`);
    for (let i = 0; i < 6; i++) { await press('j'); const k = await page.eval(`document.querySelector('.qrow.sel')?.dataset.kind`); if (k === 'triage') break; }
    check(await page.eval(`document.querySelector('.qrow.sel')?.dataset.kind`) === 'triage', `${label}: j did not reach a triage card`);
    const key = await page.eval(`document.querySelector('.qrow.sel').dataset.k`);
    const before = info.queue.count;
    await press('1');
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
    await page.eval(`window.select('gh-774')`); await press('1');
    await page.eval(`window.select('gh-789')`); await press('1');
    await page.eval(`window.select('gh-812')`); await press('3');
    info = await page.eval('window.intakeInfo()');
    for (const k of ['retried', 'approved', 'rejected']) check(info.localEvents.includes(k), `${label}: no ${k} record after the action`);
    check(info.queue.count === before - 1 - 1 - 1 + (await page.eval(`window.intakeInfo().queue.kinds.merge ?? 0`)), `${label}: the queue count after three more actions is ${info.queue.count}`);
    await page.eval(`window.setView('queue')`);
    // Scrubbed back, actions are off.
    await page.eval(`window.scrubTo(+document.getElementById('scrub').min + 3600)`);
    check(await page.eval(`window.act('1')`) === false, `${label}: an action ran while scrubbed back`);

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
    check(await page.eval(`!!document.querySelector('#panel [data-act="request_review"]')`), `${label}: no request review action on an unassigned PR`);
    check(await page.eval(`document.getElementById('panel').textContent.includes("handoff")`), `${label}: the PR card has no agent handoff`);
    await page.eval(`window.select('ado-4383')`);
    check(await page.eval(`!!document.querySelector('#panel [data-act="merged"]')`), `${label}: no merge action on an approved PR`);

    // One verdict and one set of counts: footer, Metrics headline, Board and header agree.
    label = `${size} consistency`;
    for (const f of ['fixture-work.json', 'fixture-work-capped.json']) {
      await open(f, 'view=board');
      const foot = await page.eval(`document.getElementById('bind').textContent`);
      const hot = await page.eval(`document.querySelector('.col.binding')?.dataset.stage`);
      info = await page.eval('window.intakeInfo()');
      const reviewN = info.colInfo.review.count;
      const chipN = +(await page.eval(`document.getElementById('capchip').textContent`)).match(/(\d+)\/\d+/)[1];
      check(reviewN === chipN && reviewN === info.review.count, `${label} ${f}: review counts differ: column ${reviewN}, chip ${chipN}, info ${info.review.count}`);
      check(foot.startsWith('Binding stage,') && foot.includes(`, ${info.binding}:`), `${label} ${f}: footer "${foot}" does not lead with the binding stage ${info.binding}`);
      check(foot.indexOf('Binding stage') < foot.search(/Growing|Draining/), `${label} ${f}: growth comes before the binding stage`);
      check(await page.eval(`window.stageOf && true`) && hot === info.bindingStage, `${label} ${f}: the Board highlights ${hot}, the binding stage is ${info.bindingStage}`);
      await page.eval(`window.setView('metrics')`);
      const head = await page.eval(`document.getElementById('headline').textContent`);
      check(head.includes(`Binding stage, ${info.binding}:`), `${label} ${f}: the Metrics headline names a different stage: ${head}`);
      check(head.includes(`${reviewN} ${reviewN === 1 ? 'PR' : 'PRs'} in review`), `${label} ${f}: the Metrics headline count is not ${reviewN}: ${head}`);
    }
    await open('fixture-work.json', 'view=board');
    await page.eval(`window.select('gh-803')`);
    const unb = await page.eval(`document.querySelector('#panel [data-act="unblock"]').textContent + '|' + document.querySelector('#panel .blk').textContent`);
    check(/Unblock #799/.test(unb) && unb.includes('which waits on #798') && unb.includes('marks #799 resolved'), `${label}: Unblock does not say which dependency it resolves: ${unb}`);

    // Metrics: every chart is present, and nothing is per person.
    label = `${size} metrics`;
    await open('fixture-work.json', 'view=metrics');
    check(page.problems.length === 0, `${label}: page errors: ${page.problems.join('; ')}`);
    const m = await page.eval(`document.getElementById('v-metrics').textContent`);
    for (const t of ['Waiting on a human', 'First human action', 'Cost and effort per merged fix', 'Time in each stage', 'Throughput against arrivals', 'Cumulative flow', 'Blocked time', 'Rework', 'by task class', 'merged after human edits', 'reverted in 14d']) check(m.includes(t), `${label}: missing ${t}`);
    check(await page.eval(`document.querySelectorAll('#v-metrics svg').length >= 4`), `${label}: fewer than four charts`);
    check(!/\b(dana|kim|ravi)\b/i.test(m), `${label}: a person is named in the metrics`);
    check(await page.eval(`document.documentElement.scrollWidth <= innerWidth`), `${label}: the page scrolls sideways`);


    // Decision replay: paths, the story, the explanation at a step, the fork and the redo limit.
    label = `${size} replay`;
    await open('fixture-work-replay.json', 'view=board&autoclaim=0');
    check(page.problems.length === 0, `${label}: page errors: ${page.problems.join('; ')}`);
    check(await page.eval(`document.querySelector('#c-review .row[data-k="gh-792"] .mk.paths')?.textContent === '2 paths'`), `${label}: the review card for gh-792 has no "2 paths" mark`);
    check(await page.eval(`document.querySelector('#c-needs .row[data-k="gh-795"] .mk.paths')?.textContent === '4 paths'`), `${label}: the redo-limit card has no "4 paths" mark`);
    check(await page.eval(`document.querySelector('#c-needs').textContent.includes('redo limit')`), `${label}: Needs you does not group the redo-limit item`);
    check(await page.eval(`/proposed by w\\d/.test(document.querySelector('#c-triage').textContent)`), `${label}: triage cards do not say which agent proposed`);
    check(!/pawprint/i.test(await page.eval(`document.body.innerText`)), `${label}: internal wording in the UI`);
    // Opening an item with history shows its story.
    await page.eval(`window.select('gh-792')`);
    await press('Enter');
    await new Promise((ok) => setTimeout(ok, 150));
    info = await page.eval('window.replayInfo()');
    check(info.view === 'replay', `${label}: Enter on an item with a history did not open its story`);
    const groups = await page.eval(`[...document.querySelectorAll('.grp > h4')].map((h) => h.firstChild.textContent)`);
    check(['Understand', 'Reproduce', 'Cause', 'Fix', 'Verify'].every((g) => groups.includes(g)), `${label}: story groups are ${groups}`);
    check(info.flags.some((f) => f.kind === 'tests') && info.flags.some((f) => f.kind === 'edit'), `${label}: claim-versus-step flags are missing: ${JSON.stringify(info.flags)}`);
    check(await page.eval(`document.querySelector('.flagstrip')?.textContent.includes('no test ran after the last edit')`), `${label}: the tests-claimed flag does not say what is missing`);
    // The fork sits at the third decision, in the same column as its first dot.
    const f = info.forks[0];
    const forkDecision = await page.eval(`window.__third = [...document.querySelectorAll('#path .lane[data-n="1"] .dot')][2].dataset.o`);
    check(info.forks.length === 1 && String(f.at) === forkDecision, `${label}: the fork is at #${f.at}, the third decision is #${forkDecision}`);
    check(info.dots[f.at]?.slot === 3 && info.firstDot[2].slot === 3, `${label}: fork slots are ${info.dots[f.at]?.slot} and ${info.firstDot[2].slot}, expected 3`);
    check(Math.abs(f.x1 - f.x2) < 3 && Math.abs(info.dots[f.at].x - info.firstDot[2].x) < 3, `${label}: the redo branch does not start under decision 3`);
    check(info.paths[1].parent === 1 && String(info.paths[1].at) === forkDecision && info.paths[1].by === 'dana', `${label}: the branch does not record who steered and where`);
    // Dragging shows nothing; letting go shows the panel; every sentence carries an offset.
    await page.eval(`(() => { const r = document.getElementById('rscrub'); r.value = 4; r.dispatchEvent(new Event('input', { bubbles: true })); })()`);
    check(await page.eval(`document.getElementById('explain').classList.contains('dragging') && document.querySelectorAll('#explain .sent').length === 0`), `${label}: the panel shows text while dragging`);
    await page.eval(`(() => { const r = document.getElementById('rscrub'); r.dispatchEvent(new Event('change', { bubbles: true })); })()`);
    info = await page.eval('window.replayInfo()');
    check(info.sentences.length >= 3 && info.sentences.every((x) => x.cites >= 1), `${label}: a sentence in the panel has no offset: ${JSON.stringify(info.sentences)}`);
    for (const h of ['So far', 'Right now', 'Why']) check(info.explainText.includes(h), `${label}: the panel lacks "${h}"`);
    // Arrow keys step and explain at once.
    const idx0 = (await page.eval('window.replayInfo()')).idx;
    await press('ArrowRight');
    check((await page.eval('window.replayInfo()')).idx === idx0 + 1 && (await page.eval('window.replayInfo()')).sentences.length >= 3, `${label}: the arrow key did not step and explain`);
    // No hindsight: nothing after the step is used, even a record whose text would leak.
    const leak = { o: 9999999, kind: 'decision', item: 'gh-792', attempt: 2, agent: 'w5', group: 'fix', q: 'LEAKMARKER?', options: ['LEAKMARKER'], chosen: 'LEAKMARKER', why: 'LEAKMARKER', cites: [1], so_far: [{ text: 'LEAKMARKER', cites: [1] }], claims: [] };
    const decs = await page.eval(`(() => { const out = []; for (const n of [1, 2]) for (const e of document.querySelectorAll('#path .lane[data-n="' + n + '"] .dot')) out.push([n, +e.dataset.o]); return out; })()`);
    for (const [n, o] of decs) {
      const ex = await page.eval(`(() => { const x = window.explainAt('gh-792', ${n}, ${o}, [${JSON.stringify(leak)}]); return { text: x.text, cites: x.cites, last: x.last }; })()`);
      check(!ex.text.includes('LEAKMARKER'), `${label}: the explanation at #${o} used a later record`);
      check(ex.cites.length > 0 && ex.cites.every((c) => c <= ex.last), `${label}: the explanation at #${o} cites an offset after it`);
    }
    const early = await page.eval(`window.explainAt('gh-792', 1, ${decs[1][1]}).text`);
    check(!early.includes('Add one to the sent offset') && !early.includes('Open the PR'), `${label}: the explanation at decision 2 mentions a later decision`);
    // Between two decisions the nearest earlier one is shown.
    const mid = await page.eval(`window.explainAt('gh-792', 1, ${decs[1][1]} + 1).text`);
    check(mid.includes('unit test on the resume path'), `${label}: a step after decision 2 does not show decision 2`);
    // Earlier human steering stays on the path: attempt 2's explanation and handoff carry dana's steer.
    const steer = await page.eval(`(() => { const x = window.explainAt('gh-792', 2, ${decs.find(([n]) => n === 2)[1]}); return { text: x.text, by: x.soFar.filter((l) => l.by).map((l) => l.by), cites: x.soFar.filter((l) => l.by).flatMap((l) => l.cites) }; })()`);
    check(steer.by.join() === 'dana' && steer.cites.length === 1 && steer.text.includes('The broker is right'), `${label}: attempt 2's "So far" lost dana's steer`);
    await page.eval(`window.openReplay('gh-792')`);
    await page.eval(`(() => { const r = document.getElementById('rscrub'); r.value = 9; r.dispatchEvent(new Event('change', { bubbles: true })); })()`);
    check(await page.eval(`[...document.querySelectorAll('#explain .sent')].some((p) => p.querySelector('.who')?.textContent === 'dana' && p.querySelector('.cite'))`), `${label}: a steer is not shown as a person line with an offset`);
    const stripped = await page.eval(`window.narrativeFor('gh-792', ${decs.find(([n]) => n === 2)[1]}, false).map((l) => ({ by: l.by, text: l.text, cites: l.cites }))`);
    check(stripped.length === 1 && stripped[0].by === 'dana' && stripped[0].cites.length === 1, `${label}: the dialog does not add the person's steer by itself: ${JSON.stringify(stripped)}`);
    // Questions are work items: one answered with offsets, and a new one waits for an agent.
    check(info.questions.length === 1 && info.questions[0].answered && info.questions[0].cites.length === 2, `${label}: the answered question or its cited offsets are missing`);
    await page.eval(`(() => { document.getElementById('qtext').value = 'Does this affect the Python client?'; document.getElementById('qform').requestSubmit(); })()`);
    check(await page.eval(`document.querySelector('.qa .mk.q')?.textContent.includes('waiting for an agent')`), `${label}: a posted question does not show as waiting for an agent`);
    // Redo: a new attempt is published to Ready with its handoff, then an agent claims it.
    await page.eval(`window.setView('replay')`);
    check(await page.eval(`window.redoFrom(3, 'Fix it in the client.')`) === true, `${label}: redo from decision 3 failed`);
    info = await page.eval('window.replayInfo()');
    check(info.redos === 2 && info.paths.length === 3 && info.state === 'ready' && info.lastPathState === 'published', `${label}: after a redo the item is ${info.state}, ${info.paths.length} paths`);
    check(info.handoffs.includes(3), `${label}: no handoff artifact for attempt 3`);
    const gaps = await page.eval(`window.handoffGaps('gh-792')`);
    check(gaps.length === 0, `${label}: a handoff leaves out a person's steer on its path: ${JSON.stringify(gaps)}`);
    check(await page.eval(`window.handoffGaps('gh-795').length`) === 0, `${label}: a fixture handoff leaves out a person's steer`);
    check(info.paths.filter((b) => ['running', 'review', 'draft', 'published'].includes(b.state)).length <= 2, `${label}: more than two attempts are open`);
    check(info.paths[0].state === 'closed' && info.paths[1].state === 'draft', `${label}: the earlier attempts are ${info.paths.map((b) => b.state)}`);
    await page.eval(`window.setView('board')`);
    check(await page.eval(`!!document.querySelector('#c-ready .row[data-k="gh-792"] .mk.handoff')`), `${label}: the new attempt is not in Ready with a handoff chip`);
    await page.eval(`document.querySelector('#c-ready .row[data-k="gh-792"] .mk.handoff').click()`);
    check(await page.eval(`document.querySelector('#modal.open')?.textContent.includes('Handoff for attempt 3') && document.querySelector('#modal').textContent.includes('Fix it in the client.')`), `${label}: the handoff chip does not open the artifact`);
    await press('Escape');
    check(await page.eval(`window.claimRedo('gh-792', 3)`) === true && await page.eval(`window.stageOf('gh-792')`) === 'held', `${label}: the claimed redo is not Held`);
    check(await page.eval(`!!document.querySelector('#c-held .row[data-k="gh-792"] .mk.handoff')`), `${label}: the Held card lost its handoff chip`);
    // Redo count stops at the limit.
    await page.eval(`window.openReplay('gh-792')`);
    await page.eval(`window.claimRedo('gh-792', 3)`);
    check(await page.eval(`window.redoFrom(2, 'Again.')`) === true, `${label}: the third redo failed`);
    info = await page.eval('window.replayInfo()');
    check(info.redos === 3 && info.limit === 3, `${label}: redo count is ${info.redos} of ${info.limit}`);
    const forksBefore = await page.eval(`window.intakeInfo().localEvents.filter((k) => k === 'forked_from').length`);
    check(await page.eval(`window.redoFrom(2, 'One too many.')`) === false, `${label}: a fourth redo started past the limit`);
    check(await page.eval(`window.intakeInfo().localEvents.filter((k) => k === 'forked_from').length`) === forksBefore, `${label}: a record was written past the limit`);
    check(await page.eval(`document.querySelector('[data-redo="here"]').disabled && document.getElementById('redocount').textContent === 'redo 3 of 3'`), `${label}: the redo button is not disabled at the limit`);
    // The limit item: three used, in Needs you, take it or split it.
    await open('fixture-work-replay.json', 'view=board&autoclaim=0&story=gh-795');
    info = await page.eval('window.replayInfo()');
    check(info.redos === 3 && info.state === 'needs' && info.paths.length === 4, `${label}: gh-795 is ${info.state} with ${info.redos} redos, ${info.paths.length} paths`);
    check(await page.eval(`document.querySelector('.banner.limit')?.textContent.includes('Redo limit reached, 3 of 3') && !!document.querySelector('.banner.limit [data-act="take"]') && !!document.querySelector('.banner.limit [data-act="split"]')`), `${label}: the limit banner lacks take it or split it`);
    check(await page.eval(`window.redoFrom(1, 'x')`) === false, `${label}: a redo started on the item at its limit`);
    check(await page.eval(`document.querySelectorAll('#path .lane').length`) === 4, `${label}: not every branch is drawn`);
    check(await page.eval(`document.documentElement.scrollWidth <= innerWidth`), `${label}: the story scrolls sideways`);
    await page.eval(`window.act('split')`);
    check((await page.eval('window.intakeInfo()')).localEvents.includes('split'), `${label}: split wrote no record`);
    // The side-by-side view compares the two open attempts.
    await open('fixture-work-replay.json', 'story=gh-792&tab=side&autoclaim=0');
    check(await page.eval(`document.querySelectorAll('.cmp .col2').length === 2 && document.querySelector('.cmpnote')?.textContent.includes('decision 3')`), `${label}: side by side does not compare two attempts at the fork`);
    check(await page.eval(`document.documentElement.scrollWidth <= innerWidth`), `${label}: the comparison scrolls sideways`);

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

  // Familiar conventions: the keys mean what they mean in other trackers, every
  // action is on the palette, and an action can be taken back (docs/work-tracker.md "My queue").
  await page.setViewport(1280, 800);
  let label = 'conventions';
  const key = (k, mods = {}, on = 'document') => page.eval(`${on}.dispatchEvent(new KeyboardEvent('keydown', { key: ${JSON.stringify(k)}, bubbles: true, ...${JSON.stringify(mods)} }))`);
  const ux = () => page.eval('window.ux.info()');
  const palq = `document.getElementById('palq')`;
  await open('fixture-work.json', 'view=list');
  check(page.problems.length === 0, `${label}: page errors: ${page.problems.join('; ')}`);
  const listed = await page.eval(`document.querySelector('#vtitle span').textContent`);
  check(await page.eval(`document.querySelectorAll('#list .lgroup').length`) === STAGES.length, `${label}: the list does not group by the board's stages`);
  check(await page.eval(`document.querySelectorAll('#sidebar .sn').length >= 6 && !!document.querySelector('#sidebar [data-sv]') && !!document.querySelector('#sidebar [data-src]')`), `${label}: the sidebar lacks views or sources`);
  // Ctrl+K finds a place by name and goes there.
  await key('k', { ctrlKey: true });
  let u = await ux();
  check(u.pal?.mode === 'all' && u.pal.items.includes('My queue') && u.pal.items.includes('New item'), `${label}: Ctrl+K did not open the palette with places and actions`);
  await page.eval(`window.ux.type('as a board')`);
  await key('Enter', {}, palq);
  u = await ux();
  check(u.view === 'board' && !u.pal, `${label}: the palette did not go to the board`);
  check(await page.eval(`document.querySelector('#vtitle span').textContent`) === listed, `${label}: the list and the board count different items`);
  check(u.url.includes('view=board'), `${label}: the view is not in the URL: ${u.url}`);
  await key('g'); await key('l');
  check((await ux()).view === 'list', `${label}: g then l did not go to the list`);
  await key('?');
  check((await ux()).pal?.mode === 'keys' && await page.eval(`document.getElementById('pal').textContent.includes('Command palette')`), `${label}: ? did not show the shortcuts`);
  await key('Escape');
  await key('/');
  check(await page.eval(`document.activeElement.id`) === 'q', `${label}: / did not focus search`);
  await page.eval(`document.activeElement.blur()`);
  // x selects, Shift+j extends, and neither writes a record.
  const recordsBefore = (await ux()).records;
  await key('j'); await key('x'); await key('J', { shiftKey: true });
  u = await ux();
  check(u.checked.length === 2 && u.records === recordsBefore, `${label}: x and Shift+j selected ${u.checked.length} rows, wrote ${u.records - recordsBefore} records`);
  check(await page.eval(`document.getElementById('bulk').classList.contains('open') && document.getElementById('bulk').textContent.includes('2 selected')`), `${label}: no bulk bar for a selection`);
  // A bulk action applies to every selected row and one undo takes all of it back.
  await key('Enter', { ctrlKey: true });
  u = await ux();
  check(u.toast?.undo && /2 items/.test(u.toast.text) && u.checked.length === 0, `${label}: the bulk action gave no undo toast: ${JSON.stringify(u.toast)}`);
  check((await page.eval('window.intakeInfo()')).localEvents.filter((k) => k === 'triaged').length === 2, `${label}: the bulk accept did not write two records`);
  await key('z', { ctrlKey: true });
  u = await ux();
  check(u.records === recordsBefore && (await page.eval('window.intakeInfo()')).localEvents.length === 0, `${label}: undo left records behind`);
  // c creates, s sets status, p sets tier, a assigns: each opens its picker.
  await page.eval(`window.select('gh-774')`);
  for (const [k, mode] of [['c', 'create'], ['s', 'status'], ['p', 'tier'], ['a', 'assign'], ['f', 'filter']]) {
    await key(k);
    check((await ux()).pal?.mode === mode, `${label}: ${k} did not open the ${mode} picker`);
    await key('Escape', {}, `document.getElementById('pal')`);
  }
  await key('s'); await page.eval(`window.ux.type('parked')`); await key('Enter', {}, palq);
  check((await page.eval('window.intakeInfo()')).localEvents.includes('parked'), `${label}: the status picker did not park the item`);
  await key('z', { ctrlKey: true });
  await key('c'); await page.eval(`window.ux.type('Publish stalls after a rebalance')`); await key('Enter', {}, palq);
  check((await page.eval('window.intakeInfo()')).localEvents.includes('imported') && await page.eval(`document.getElementById('list').textContent.includes('Publish stalls after a rebalance')`), `${label}: c did not create an item in Triage`);
  // Filters narrow the list, live in the URL, and can be saved as a view.
  await page.eval(`window.ux.filter({ stage: ['review'] })`);
  u = await ux();
  check(u.url.includes('stage=review') && await page.eval(`[...document.querySelectorAll('#list .lrow')].every((r) => r.dataset.stage === 'review')`), `${label}: the status filter did not narrow the list`);
  await page.eval(`(window.ux.openPal('saveview'), window.ux.type('Reviews'), window.ux.run())`);
  u = await ux();
  check(u.views.includes('Reviews') && await page.eval(`document.getElementById('snav').textContent.includes('Reviews')`), `${label}: the saved view is not in the sidebar`);
  await page.eval(`window.ux.filter({ q: 'no such item anywhere' })`);
  check(await page.eval(`document.querySelector('#list .emptyst')?.textContent.includes('No items match')`), `${label}: no empty state for filters that match nothing`);
  // A question offers its options as numbers, the default on the main key, and free text.
  await open('fixture-work.json', 'view=queue&select=ado-4377');
  u = await ux();
  check(u.actions.filter((a) => /^opt/.test(a.id)).length === 3 && await page.eval(`document.querySelectorAll('#panel .qopts button').length === 3 && !!document.querySelector('#panel .qopts input')`), `${label}: the question card lacks its three options or the free text field`);
  check(await page.eval(`document.querySelector('.qrow.sel .opts')?.textContent.includes('felix-gateway')`), `${label}: the queue row does not show the options`);
  await key('1');
  check(await page.eval(`document.getElementById('evlist').textContent.includes('chosen=felix ')`) || await page.eval(`/chosen=felix$/.test(document.querySelector('#evlist li').textContent)`), `${label}: 1 did not answer with the first option`);
  await key('z', { ctrlKey: true });
  await page.eval(`window.select('ado-4377')`);
  await key('Enter', { ctrlKey: true });
  check(await page.eval(`document.querySelector('#evlist li').textContent.includes('chosen=felix-gateway')`), `${label}: the main key did not answer with the default`);
  await key('z', { ctrlKey: true });
  await page.eval(`(window.select('ado-4377'), document.getElementById('note').value = 'Neither: it is the Python client')`);
  await key('Enter', {}, `document.getElementById('note')`);
  check(await page.eval(`document.querySelector('#evlist li').textContent.includes('text=Neither: it is the Python client')`), `${label}: free text was not recorded as typed`);
  // Typing in a field never runs the selected item's action, and an empty Other field sends nothing.
  await open('fixture-work.json', 'view=list&select=gh-808');
  const before = (await ux()).records;
  await key('Enter', { ctrlKey: true }, `document.getElementById('q')`);
  check((await ux()).records === before, `${label}: Ctrl+Enter in the search box acted on the selected item`);
  await key('Enter', { metaKey: true }, `document.getElementById('q')`);
  check((await ux()).records === before, `${label}: Cmd+Enter in the search box acted on the selected item`);
  await open('fixture-work.json', 'view=queue&select=ado-4377');
  const q0 = (await ux()).records;
  await key('Enter', {}, `document.getElementById('note')`);
  check((await ux()).records === q0, `${label}: Enter in an empty Other field recorded the default answer`);
  check(!(await page.eval(`document.getElementById('panel').textContent`)).includes('asked before any work started'), `${label}: the question card guesses when it was asked`);
  // The palette traps focus, makes the page inert, and gives focus back.
  await open('fixture-work.json', 'view=list');
  await page.eval(`document.getElementById('newbtn').focus()`);
  await key('c');
  check(await page.eval(`document.getElementById('page').inert && document.getElementById('pal').contains(document.activeElement)`), `${label}: the palette did not take focus and make the page inert`);
  const n = await page.eval(`document.getElementById('pal').querySelectorAll('input, button').length`);
  await key('Tab', { shiftKey: true }, `document.getElementById('palq')`);
  check(await page.eval(`document.getElementById('pal').contains(document.activeElement)`), `${label}: Shift+Tab left the palette (${n} controls)`);
  await key('Escape', {}, `document.getElementById('palq')`);
  check(await page.eval(`!document.getElementById('page').inert && document.activeElement.id`) === 'newbtn', `${label}: closing the palette did not return focus to its opener`);
  await page.eval(`document.getElementById('help').focus()`);
  await page.eval(`window.ux.openPal('keys')`);
  await key('Tab', {}, `document.querySelector('#pal button')`);
  check(await page.eval(`document.getElementById('pal').contains(document.activeElement)`), `${label}: Tab left the shortcuts dialog`);
  await key('Escape', {}, `document.getElementById('pal')`);
  check(await page.eval(`document.activeElement.id`) === 'help', `${label}: closing the shortcuts did not return focus to ?`);
  // Row checkboxes and badges take focus and work from the keyboard; the list is one listbox.
  await open('fixture-work.json', 'view=list&select=gh-774');
  check(await page.eval(`(() => { const l = document.getElementById('list'); return l.getAttribute('role') === 'listbox' && document.getElementById(l.getAttribute('aria-activedescendant'))?.dataset.k === 'gh-774'; })()`), `${label}: the list does not point at its selected row`);
  check(await page.eval(`document.querySelector('#list [data-check]').matches('[tabindex]')`), `${label}: a row checkbox cannot take focus`);
  await page.eval(`document.querySelector('#list .lrow[data-k="gh-774"] [data-check]').focus()`);
  await key('Enter', {}, `document.activeElement`);
  check((await ux()).checked.includes('gh-774') && await page.eval(`document.activeElement.dataset.check`) === 'gh-774', `${label}: Enter on a checkbox did not select the row and keep focus`);
  // Requesting a review asks who, and the count is one number.
  await open('fixture-work-capped.json', 'view=queue');
  const quick = await page.eval(`document.querySelector('[data-quick="request_review"]')?.getAttribute('tabindex')`);
  check(quick === '0', `${label}: the Request review badge cannot take focus`);
  const keyOf = await page.eval(`document.querySelector('[data-quick="request_review"]').dataset.k`);
  const rr = (await ux()).records;
  await page.eval(`document.querySelector('[data-quick="request_review"]').click()`);
  u = await ux();
  check(u.pal?.mode === 'reviewer' && u.records === rr, `${label}: Request review did not ask who (${u.pal?.mode}, ${keyOf})`);
  await page.eval(`window.ux.closePal()`);
  await open('fixture-work.json', 'view=list');
  check(await page.eval(`document.querySelector('#vtitle span').textContent === document.querySelector('[data-go="work"] b').textContent`), `${label}: the title and the sidebar count different work`);
  // Text that carries meaning is readable in both themes.
  for (const theme of ['dark', 'light']) {
    await open('fixture-work.json', `view=list&theme=${theme}`);
    const ratio = await page.eval(`(() => { const lum = (c) => { const m = c.match(/[\\d.]+/g).map(Number); const v = m.slice(0, 3).map((x) => { x /= 255; return x <= .03928 ? x / 12.92 : ((x + .055) / 1.055) ** 2.4; }); return .2126 * v[0] + .7152 * v[1] + .0722 * v[2]; }; const cs = getComputedStyle(document.documentElement); const t = document.createElement('i'); t.style.color = 'var(--faint)'; document.body.append(t); const f = lum(getComputedStyle(t).color); t.remove(); const b = lum(getComputedStyle(document.body).backgroundColor); const [a, z] = f > b ? [f, b] : [b, f]; return (a + .05) / (z + .05); })()`);
    check(ratio >= 4.5, `${label}: faint text is ${ratio.toFixed(2)}:1 in the ${theme} theme, needs 4.5`);
  }
  // A refused write is put back and says so next to the item.
  await open('fixture-work.json', 'view=queue&select=gh-808&fail=1');
  await key('1');
  u = await ux();
  check(u.toast?.err && (await page.eval(`window.stageOf('gh-808')`)) === 'triage' && await page.eval(`!!document.querySelector('#panel .errline')`), `${label}: a refused action was not rolled back with an error`);
  // Empty, loading and density states.
  await open('fixture-work-quiet.json', 'view=queue');
  check(await page.eval(`document.querySelector('#qlist .emptyst')?.textContent.includes('caught up')`) && (await page.eval('window.intakeInfo()')).queue.count === 0, `${label}: an empty queue has no empty state`);
  await open('fixture-work.json', 'loading=1');
  check(await page.eval(`document.querySelectorAll('#qlist .sk').length >= 6`), `${label}: no skeleton while loading`);
  const rowHeight = async (d) => { await open('fixture-work.json', `view=list&density=${d}`); return page.eval(`document.querySelector('#list .lrow').getBoundingClientRect().height`); };
  check(await rowHeight('compact') < await rowHeight('comfortable'), `${label}: compact rows are not shorter`);
  // The chrome uses plain words.
  await page.eval(`window.ux.openPal('all')`);
  const chrome = await page.eval(`['sidebar', 'legend', 'pal'].map((id) => document.getElementById(id).innerText).join(' ') + document.querySelector('header').innerText`);
  check(!/\b(logs?|streams?|offsets?)\b/i.test(chrome), `${label}: storage wording in the navigation or the palette`);

  // The footer's verdict agrees in number and keeps "Show it" apart from the next sentence.
  for (const f of ['fixture-work.json', 'fixture-work-capped.json', 'fixture-work-burst.json', 'fixture-work-replay.json']) {
    await open(f, 'view=board');
    const bind = await page.eval(`document.getElementById('bind').textContent`);
    check(!/\b1 are\b|\b(?:[02-9]|\d\d+) is\b/.test(bind) && !/Show it(?!\s*·)/.test(bind), `${f}: footer wording: ${bind}`);
  }
  // The autoplay demo plays its whole story from the fixture, with no page errors, and loops back to where it began.
  await page.setViewport(1280, 720);
  await open('fixture-work-demo.json', 'demo=1&rec=1&loops=2');
  const start = await page.eval('window.intakeInfo()');
  await page.eval('window.demo.start()');
  let guard = 0;
  while (await page.eval('window.demo.running')) {
    await page.eval('window.demo.advance(100)');
    const bind = await page.eval(`document.getElementById('bind').textContent`);
    if (/\b1 are\b|\b(?:[02-9]|\d\d+) is\b/.test(bind) || /Show it(?!\s*·)/.test(bind)) { fail(`demo: footer wording: ${bind}`); break; }
    if (++guard > 2600) { fail('demo: did not finish within 260 s of its own time'); break; }
  }
  const demo = await page.eval('({ errors: window.demo.errors, loops: window.demo.loops, runs: window.demo.runs })');
  check(demo.errors.length === 0 && page.problems.length === 0, `demo: errors: ${[...demo.errors, ...page.problems].join('; ')}`);
  check(demo.loops === 2 && demo.runs.length === 2, `demo: expected two passes, got ${demo.loops}`);
  const [one, two] = demo.runs;
  check(one && one.seconds >= 24 && one.seconds <= 30, `demo: the story runs ${one?.seconds} s, expected 24 to 30`);
  check(one && JSON.stringify(one) === JSON.stringify(two), 'demo: the second pass differs from the first');
  for (const [kind, n] of [['triaged', 3], ['approved', 1], ['done', 1], ['human_input', 1]]) check(one && one.events.filter((k) => k === kind).length === n, `demo: expected ${n} ${kind} record(s)`);
  check(one && one.done === start.colInfo.done.count + 1, `demo: Done went from ${start.colInfo.done.count} to ${one?.done}, expected one more`);
  const end = await page.eval('window.intakeInfo()');
  check(JSON.stringify(end.colInfo) === JSON.stringify(start.colInfo) && end.localEvents.length === 0, 'demo: the loop did not return to its starting board');
} finally {
  await page.close();
  await server.close();
}
if (failures.length) process.exit(1);
console.log('intake ok');
