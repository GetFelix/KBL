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
    const unb = await page.eval(`document.querySelector('#dock [data-act="u"]').textContent + '|' + document.querySelector('#dock .blk').textContent`);
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
    check(!/kibble|pawprint/i.test(await page.eval(`document.body.innerText`)), `${label}: internal wording in the UI`);
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
    check(await page.eval(`document.querySelector('.banner.limit')?.textContent.includes('Redo limit reached, 3 of 3') && !!document.querySelector('.banner.limit [data-act="t"]') && !!document.querySelector('.banner.limit [data-act="l"]')`), `${label}: the limit banner lacks take it or split it`);
    check(await page.eval(`window.redoFrom(1, 'x')`) === false, `${label}: a redo started on the item at its limit`);
    check(await page.eval(`document.querySelectorAll('#path .lane').length`) === 4, `${label}: not every branch is drawn`);
    check(await page.eval(`document.documentElement.scrollWidth <= innerWidth`), `${label}: the story scrolls sideways`);
    await page.eval(`window.act('l')`);
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
} finally {
  await page.close();
  await server.close();
}
if (failures.length) process.exit(1);
console.log('intake ok');
