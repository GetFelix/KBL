import assert from 'node:assert/strict';
import test from 'node:test';
import type { ItemRow } from '../src/fold.ts';
import { reduce, type ViewState } from '../src/state.ts';
import { Tracker, type Fetch } from '../src/tracker.ts';
import type { DecisionLine } from '../src/story.ts';
import { acceptTier, canSetNeeds, checkNeeds, logTakesNeeds, needsPick, parseNeeds, assigneePerson, assignPick, assignTarget, CAN, canAssign, canRate, canSteer, canTakeAfterLimit, checkRedo, checkSteer, redoBlock, redoCopy, redoStale, redoNeedsNote, heldAttempt, initials, needsLine, canUndo, logTakesFlags, marksAfter, checkAnswer, jobsFor, latestFor, NO_WRITES, outcome, pending, putWrite, request, route, shownTier, summary, UNDO, undoFor, undoRequest, type Write, type Writes } from '../src/writes.ts';

const item = (o: Partial<ItemRow> & { id: string }): ItemRow => ({ title: o.id, stage: 'ready', tier: 1, format: 7, ...o });

test('each action is offered only where the fold would take it', () => {
  const triage = item({ id: 't', stage: 'triage', tier: null });
  const proposed = item({ id: 'p', stage: 'proposed' });
  const done = item({ id: 'd', stage: 'done' });
  const asked = item({ id: 'q', stage: 'needs_you', needs: 'question', question: 7 });
  const failed = item({ id: 'f', stage: 'needs_you', needs: 'check_failed' });
  assert.deepEqual([CAN.accept(triage), CAN.accept(proposed), CAN.accept(done)], [true, true, false]);
  assert.deepEqual([CAN.reject(triage), CAN.reject(proposed)], [false, true]);
  assert.deepEqual([CAN.close(done), CAN.tier(done), CAN.close(asked)], [false, false, true]);
  assert.deepEqual([CAN.answer(asked), CAN.answer(failed)], [true, false]);
});

test('requests are the tracker person routes, under the team when one is named', () => {
  assert.deepEqual(request('a', { kind: 'accept', tier: 2 }), { path: '/v1/work/proposals/accept', body: { item: 'a', tier: 2 } });
  assert.deepEqual(request('a', { kind: 'accept', tier: null }), { path: '/v1/work/proposals/accept', body: { item: 'a' } });
  assert.deepEqual(request('a', { kind: 'reject' }), { path: '/v1/work/proposals/reject', body: { item: 'a' } });
  assert.deepEqual(request('a', { kind: 'tier', tier: 0 }), { path: '/v1/work/tier', body: { item: 'a', tier: 0 } });
  assert.deepEqual(request('a', { kind: 'close', why: 'dismissed' }), { path: '/v1/work/close', body: { item: 'a', why: 'dismissed' } });
  assert.deepEqual(request('a', { kind: 'answer', question: 4, text: '  ', chosen: 'rust' }), {
    path: '/v1/work/questions/answer',
    body: { item: 'a', question: 4, chosen: 'rust' },
  });
  // Inline text is content, `{ text }`: a bare string is refused by the tracker.
  assert.deepEqual(request('a', { kind: 'answer', question: 4, text: ' yes ', chosen: null }).body, { item: 'a', question: 4, text: { text: 'yes' } });
  assert.equal(route('/v1/work/tier', null), '/v1/work/tier');
  assert.equal(route('/v1/work/tier', 'a b'), '/v1/teams/a%20b/work/tier');
});

test('an answer form names what is missing before anything is sent', () => {
  assert.equal(checkAnswer([], '', null), 'Write an answer.');
  assert.equal(checkAnswer(['a', 'b'], '', null), 'Pick an option, or write an answer.');
  assert.equal(checkAnswer(['a'], '', 'a'), null);
  assert.equal(checkAnswer(['a'], '', 'gone'), 'That option is no longer offered.');
  // The tracker refuses an answer with both an option and text.
  assert.match(checkAnswer(['a'], 'and also', 'a')!, /not both/);
  assert.match(checkAnswer([], 'x'.repeat(5000), null)!, /4 KiB/);
});

test('a refusal keeps the tracker’s own reason and code', () => {
  assert.deepEqual(outcome(200, { kind: 'accepted', offset: 12 }), { status: 'ok', offset: 12 });
  assert.deepEqual(outcome(409, { error: 'the fold refused the record: not a proposal', why: 'not_a_proposal' }), {
    status: 'refused',
    why: 'not_a_proposal',
    reason: 'the fold refused the record: not a proposal',
  });
  assert.equal((outcome(401, null) as { why: string }).why, 'signed_out');
  // Only a 401 or 403 signs the page out, never a fold refusal that happens to say so.
  assert.equal((outcome(401, null) as { signedOut?: true }).signedOut, true);
  assert.equal((outcome(403, null) as { signedOut?: true }).signedOut, true);
  assert.equal((outcome(409, { error: 'x', why: 'forbidden' }) as { signedOut?: true }).signedOut, undefined);
  assert.equal(outcome(503, { error: 'team busy' }).status, 'failed');
  assert.match((outcome(null, null, 'Failed to fetch') as { reason: string }).reason, /Cannot reach the tracker: Failed to fetch/);
});

test('only a tier change can be undone, and only while nobody changed it again', () => {
  const before = item({ id: 'a', tier: 1 });
  const u = undoFor(before, 3);
  assert.deepEqual(u, { item: 'a', tier: 1, set: 3 });
  assert.match(undoFor(item({ id: 'b', tier: null }), 2) as string, /no record removes a tier/);
  if (typeof u === 'string') throw new Error('expected an undo');
  // The undo is a new tier_changed record setting the old tier back.
  assert.deepEqual(undoRequest(u, [item({ id: 'a', tier: 3 })]), { path: '/v1/work/tier', body: { item: 'a', tier: 1 } });
  assert.match(undoRequest(u, [item({ id: 'a', tier: 0 })]) as string, /changed again since/);
  assert.match(undoRequest(u, [item({ id: 'a', tier: 3, stage: 'closed' })]) as string, /closed now/);
  assert.match(undoRequest(u, []) as string, /no longer in the log/);
  // Until the change comes back on the log there is nothing to undo yet.
  assert.match(undoRequest(u, [item({ id: 'a', tier: 1 })]) as string, /not come back/);
  const ok = { id: 1, batch: 1, kind: 'tier' as const, item: 'a', status: 'ok' as const, undo: u };
  assert.deepEqual([canUndo([ok], [item({ id: 'a', tier: 1 })]), canUndo([ok], [item({ id: 'a', tier: 3 })])], [false, true]);
});

test('the write list moves from pending to a result and says so in one line', () => {
  const w = (o: Partial<Write> & { id: number; item: string }): Write => ({ batch: 1, kind: 'tier', status: 'pending', ...o });
  let ws = putWrite(NO_WRITES, w({ id: 1, item: 'a', undo: { item: 'a', tier: 1, set: 2 } }));
  ws = putWrite(ws, w({ id: 2, item: 'b' }));
  assert.equal(pending(ws, 'a'), true);
  assert.deepEqual(summary(ws.list), { text: 'Setting the tier of 2 items…', tone: 'busy', undoable: 0 });
  ws = putWrite(ws, { ...ws.list[0], status: 'ok' });
  ws = putWrite(ws, { ...ws.list[1], status: 'refused', reason: 'the item is closed', why: 'closed' });
  assert.equal(pending(ws, 'a'), false);
  assert.equal(latestFor(ws, 'b')?.status, 'refused');
  assert.deepEqual(summary(ws.list), { text: 'Set the tier of a. b: refused, the item is closed (closed)', tone: 'error', undoable: 1 });
  assert.equal(ws.last, 1);
  // A slower, older batch finishing late does not take the activity line back.
  const later = putWrite(putWrite(ws, w({ id: 3, item: 'c', batch: 2 })), { ...ws.list[0], batch: 1, status: 'failed', reason: 'x' });
  assert.equal(later.last, 2);
});

test('marks toggle, and a mark on an item the log no longer has is dropped', () => {
  const s = { marked: [], items: [item({ id: 'a' }), item({ id: 'b' })], writes: NO_WRITES } as unknown as ViewState;
  let n = reduce(s, { type: 'mark', id: 'a' });
  n = reduce(n, { type: 'mark', id: 'b' });
  assert.deepEqual(n.marked, ['a', 'b']);
  n = reduce(n, { type: 'mark', id: 'a' });
  assert.deepEqual(n.marked, ['b']);
  n = reduce(n, { type: 'items', items: [item({ id: 'a' })], blocked: [], overlaps: new Map() });
  assert.deepEqual(n.marked, []);
});

test('the client sends the viewer’s token to the page’s own routes, and reads refusals', async () => {
  const seen: { url: string; auth: string; body?: string }[] = [];
  const fake: Fetch = async (url, init) => {
    seen.push({ url, auth: init.headers.authorization, body: init.body });
    if (url === '/v1/me') return { status: 200, json: async () => ({ person: 'sam', teams: [{ team: 'default', role: 'admin' }] }) };
    return { status: 409, json: async () => ({ error: 'not the open question', why: 'not_open_question' }) };
  };
  const t = new Tracker(fake, 'tok', 'default');
  assert.deepEqual(await t.me(), { person: 'sam', teams: ['default'], limits: {} });
  const o = await t.write(request('q', { kind: 'answer', question: 3, text: 'yes', chosen: null }));
  assert.deepEqual(o, { status: 'refused', why: 'not_open_question', reason: 'not the open question' });
  assert.deepEqual(seen.map((x) => [x.url, x.auth]), [['/v1/me', 'Bearer tok'], ['/v1/teams/default/work/questions/answer', 'Bearer tok']]);
  const down = new Tracker(async () => Promise.reject(new Error('Failed to fetch')), 'tok', null);
  assert.equal((await down.write(request('a', { kind: 'reject' }))).status, 'failed');
});

test('the assign picker reads the team’s agents from the tracker', async () => {
  const seen: string[] = [];
  const fake: Fetch = async (url) => {
    seen.push(url);
    return { status: 200, json: async () => ({ agents: ['w1', 'w2', 7] }) };
  };
  assert.deepEqual(await new Tracker(fake, 'tok', 'default').agents(), ['w1', 'w2']);
  assert.deepEqual(seen, ['/v1/teams/default/work/agents']);
  const garbled = new Tracker(async () => ({ status: 200, json: async () => ({}) }), 'tok', null);
  assert.equal(((await garbled.agents()) as { status: string }).status, 'failed');
  const refused = new Tracker(async () => ({ status: 403, json: async () => ({ error: 'persons only' }) }), 'tok', null);
  assert.equal(((await refused.agents()) as { status: string }).status, 'refused');
});

test('a write that gets no answer is abandoned as failed, with its outcome unknown', async () => {
  const hung: Fetch = (_url, init) => new Promise((_res, rej) => init.signal?.addEventListener('abort', () => rej(new DOMException('aborted', 'AbortError'))));
  const t = new Tracker(hung, 'tok', null, 20);
  const o = await t.write(request('a', { kind: 'reject' }));
  assert.equal(o.status, 'failed');
  assert.match((o as { reason: string }).reason, /No answer from the tracker; it may or may not have been written/);
  const line = summary([{ id: 1, batch: 1, kind: 'reject', item: 'a', status: 'failed', reason: (o as { reason: string }).reason }]);
  assert.match(line.text, /^a: failed, No answer from the tracker/);
});

test('the Needs you inbox is a view in the address that holds only items waiting on a person', async () => {
  const { decodeView, encodeView } = await import('../src/views.ts');
  const { inScope } = await import('../src/list.ts');
  assert.equal(decodeView('view=inbox').view, 'inbox');
  assert.equal(encodeView('', { view: 'inbox', layout: 'list', query: '', filters: {} }), 'view=inbox');
  const items = [item({ id: 'a', stage: 'needs_you', needs: 'question' }), item({ id: 'b' })];
  assert.deepEqual(inScope({ items, blocked: [], view: 'inbox' }).map((i) => i.id), ['a']);
  // A bulk action reaches only the marks the view shows.
  const { markedShown } = await import('../src/list.ts');
  const marks = { items, blocked: [], view: 'all' as const, query: 'b', filters: {}, marked: ['a', 'b'] };
  assert.deepEqual(markedShown(marks).map((i) => i.id), ['b']);
});

test('a bulk write over many items keeps every write until its batch is old', () => {
  const ids = Array.from({ length: 80 }, (_, i) => `i${i}`);
  let ws: Writes = NO_WRITES;
  for (const [n, item] of ids.entries()) ws = putWrite(ws, { id: n + 1, batch: 1, kind: 'tier', item, status: 'pending', undo: { item, tier: 1, set: 3 } });
  // The first item's write is still known, so a second click on it is not sent again.
  assert.equal(pending(ws, 'i0'), true);
  for (const w of [...ws.list]) ws = putWrite(ws, w.item === 'i3' ? { ...w, status: 'refused', reason: 'the item is closed', why: 'closed', undo: undefined } : { ...w, status: 'ok' });
  const batch = ws.list.filter((w) => w.batch === 1);
  assert.equal(batch.length, 80);
  const line = summary(batch);
  assert.equal(line.text, 'Set the tier of 79 items. i3: refused, the item is closed (closed)');
  assert.equal(line.undoable, 79);
  // Later batches push out old ones whole, never a pending write.
  ws = putWrite(ws, { id: 1000, batch: 2, kind: 'close', item: 'p', status: 'pending' });
  for (let b = 3; b <= 12; b++) ws = putWrite(ws, { id: 1000 + b, batch: b, kind: 'tier', item: `b${b}`, status: 'ok' });
  assert.equal(ws.list.some((w) => w.batch === 1), false);
  assert.equal(pending(ws, 'p'), true);
});

test('a tier field starts from the recorded or proposed tier, and an accept sends the tier shown', () => {
  const tri = item({ id: 't', stage: 'triage', tier: null, proposed_tier: 0 });
  assert.equal(shownTier(tri), 0);
  assert.equal(shownTier(item({ id: 'n', stage: 'triage', tier: null })), null);
  // Submitted untouched at the shown tier 0, it is sent, so a later re-proposal cannot replace it.
  assert.equal(acceptTier('0'), 0);
  assert.equal(acceptTier('3'), 3);
  // Nothing shown, or "keep each item's tier": none sent.
  assert.equal(acceptTier(''), null);
});

test('a write is checked against the log when it is sent, not when its dialog opened', () => {
  const now = [item({ id: 'a', stage: 'ready', tier: 1 }), item({ id: 'b', stage: 'triage', tier: null }), item({ id: 'c', stage: 'ready', tier: null })];
  // `a` was accepted by someone else while the confirm was open; `gone` left the log.
  const jobs = jobsFor(['a', 'b', 'gone'], { kind: 'accept', tier: 2 }, now, NO_WRITES);
  assert.match(jobs[0].req as string, /^a changed since: it is ready now/);
  assert.deepEqual(jobs[1].req, { path: '/v1/work/proposals/accept', body: { item: 'b', tier: 2 } });
  assert.match(jobs[2].req as string, /no longer in the log/);
  // A first tier has no Undo, and the activity line says why.
  const [first] = jobsFor(['c'], { kind: 'tier', tier: 0 }, now, NO_WRITES);
  assert.equal(first.undo, undefined);
  const line = summary([{ id: 1, batch: 1, kind: 'tier', item: 'c', status: 'ok', noUndo: first.noUndo }]);
  assert.equal(line.text, 'Set the tier of c. No undo for c: it had no tier before, and no record removes a tier.');
  assert.equal(line.undoable, 0);
  // An item with a write in flight is left out.
  const busy = putWrite(NO_WRITES, { id: 1, batch: 1, kind: 'tier', item: 'c', status: 'pending' });
  assert.deepEqual(jobsFor(['c'], { kind: 'tier', tier: 0 }, now, busy), []);
});

test('flag and unflag are offered only where the fold would take them', () => {
  const ready = item({ id: 'r', accepted: true });
  const flagged = item({ id: 'f', stage: 'needs_you', needs: 'flagged', accepted: true, flagged: true });
  // Flagged but waiting for a question: the question shows, yet the flag can still be cleared.
  const both = item({ id: 'b', stage: 'needs_you', needs: 'question', accepted: true, flagged: true });
  const triage = item({ id: 't', stage: 'triage', accepted: false });
  const done = item({ id: 'd', stage: 'done', accepted: true, flagged: true });
  assert.deepEqual([ready, flagged, both, triage, done].map((i) => CAN.flag(i)), [true, false, false, false, false]);
  assert.deepEqual([ready, flagged, both, triage, done].map((i) => CAN.unflag(i)), [false, true, true, false, false]);
});

test('flag and unflag are the tracker person routes, with no confirm', () => {
  assert.deepEqual(request('a', { kind: 'flag' }), { path: '/v1/work/flag', body: { item: 'a' } });
  assert.deepEqual(request('a', { kind: 'unflag' }), { path: '/v1/work/unflag', body: { item: 'a' } });
  assert.equal(route('/v1/work/flag', 'ops'), '/v1/teams/ops/work/flag');
  assert.equal(UNDO.flag, null);
  assert.equal(UNDO.unflag, null);
});

test('a flag on an item that changed since is refused unsent, and the refusal reads in words', () => {
  const was = item({ id: 'a', accepted: true });
  const now = item({ id: 'a', stage: 'needs_you', needs: 'flagged', accepted: true, flagged: true });
  const [stale] = jobsFor(['a'], { kind: 'flag' }, [now], NO_WRITES);
  assert.match(stale.req as string, /already flagged/);
  const [again] = jobsFor(['a'], { kind: 'unflag' }, [was], NO_WRITES);
  assert.match(again.req as string, /not flagged/);
  const closed = item({ id: 'a', stage: 'closed', accepted: true, flagged: true });
  assert.doesNotMatch(jobsFor(['a'], { kind: 'flag' }, [closed], NO_WRITES)[0].req as string, /already flagged/);
  const [ok] = jobsFor(['a'], { kind: 'flag' }, [was], NO_WRITES);
  assert.deepEqual(ok.req, { path: '/v1/work/flag', body: { item: 'a' } });
});

test('the tracker’s no_change and not_accepted refusals show with their code', () => {
  const o = outcome(409, { error: 'the fold refused the record: nothing to change', why: 'no_change' });
  assert.deepEqual(o, { status: 'refused', why: 'no_change', reason: 'the fold refused the record: nothing to change' });
  const line = summary([{ id: 1, batch: 1, kind: 'flag', item: 'a', status: 'refused', reason: (o as { reason: string }).reason, why: 'no_change' }]);
  assert.equal(line.tone, 'error');
  assert.match(line.text, /a: refused, the fold refused the record: nothing to change \(no_change\)/);
  assert.equal(summary([{ id: 1, batch: 1, kind: 'unflag', item: 'a', status: 'ok' }]).text, 'Unflagged a.');
});

test('flag and unflag are hidden on a log below format 7', () => {
  const old = { format: 6 };
  assert.equal(CAN.flag(item({ id: 'r', accepted: true, ...old })), false);
  assert.equal(CAN.unflag(item({ id: 'f', accepted: true, flagged: true, ...old })), false);
  assert.equal(logTakesFlags([item({ id: 'a', ...old })]), false);
  assert.equal(logTakesFlags([item({ id: 'a' })]), true);
  // Other writes do not depend on the format.
  assert.equal(CAN.close(item({ id: 'c', ...old })), true);
});

test('a refusal for the log format says how to fix it', () => {
  const w: Write = { item: 'a', kind: 'flag', batch: 1, status: 'refused', reason: 'raw', why: 'format' } as Write;
  const line = summary([w]);
  assert.match(line.text, /raise the log to format 7 to use Needs-you flags/);
  assert.doesNotMatch(line.text, /\(format\)/);
});

test('acting on part of the selection keeps the other marks', () => {
  assert.deepEqual(marksAfter(['a', 'b', 'c'], ['a', 'b']), ['c']);
  assert.deepEqual(marksAfter(['a', 'b'], ['a', 'b']), []);
  assert.deepEqual(marksAfter(['a', 'b'], ['z']), ['a', 'b']);
  assert.deepEqual(marksAfter(['a'], []), ['a']);
});

test('me carries each team’s review limit, and leaves out a team with none', async () => {
  const fake: Fetch = async () => ({ status: 200, json: async () => ({ person: 'sam', teams: [{ team: 'a', review_limit: 3 }, { team: 'b', review_limit: 0 }, { team: 'c', review_limit: null }] }) });
  const me = await new Tracker(fake, 'tok', null).me();
  assert.deepEqual('limits' in me && me.limits, { a: 3 });
});

test('a summary mark is the tracker person route, and is offered only on a log at format 11', () => {
  assert.deepEqual(request('a', { kind: 'rate', attempt: 2, accurate: false }), { path: '/v1/work/rate-summary', body: { item: 'a', attempt: 2, accurate: false } });
  assert.equal(route('/v1/work/rate-summary', 'ops'), '/v1/teams/ops/work/rate-summary');
  assert.equal(UNDO.rate, null);
  // A closed item's summary can still be marked; a log below 11 refuses the record.
  assert.deepEqual([item({ id: 'a', format: 10 }), item({ id: 'a', format: 11 }), item({ id: 'a', stage: 'done', format: 11 })].map(canRate), [false, true, true]);
  const [old] = jobsFor(['a'], { kind: 'rate', attempt: 1, accurate: true }, [item({ id: 'a', format: 10 })], NO_WRITES);
  assert.match(old.req as string, /below format 11/);
  const [ok] = jobsFor(['a'], { kind: 'rate', attempt: 1, accurate: true }, [item({ id: 'a', stage: 'done', format: 11 })], NO_WRITES);
  assert.deepEqual(ok.req, { path: '/v1/work/rate-summary', body: { item: 'a', attempt: 1, accurate: true } });
});

const twelve = { format: 12, accepted: true };

test('assign, take and steer are the tracker person routes', () => {
  assert.deepEqual(request('a', { kind: 'take' }), { path: '/v1/work/take', body: { item: 'a' } });
  assert.deepEqual(request('a', { kind: 'reassign', to: 'ana' }), { path: '/v1/work/reassign', body: { item: 'a', to: { person: 'ana' } } });
  assert.deepEqual(request('a', { kind: 'reassign', to: null }), { path: '/v1/work/reassign', body: { item: 'a', to: null } });
  assert.deepEqual(request('a', { kind: 'reassign', to: 'w1', agent: true }), { path: '/v1/work/reassign', body: { item: 'a', to: { agent: 'w1' } } });
  assert.deepEqual(request('a', { kind: 'steer', attempt: 2, text: ' slow down ' }), { path: '/v1/work/steer', body: { item: 'a', attempt: 2, text: { text: 'slow down' } } });
  assert.equal(route('/v1/work/take', 'ops'), '/v1/teams/ops/work/take');
});

test('assigning is offered only on a log at format 12, on an accepted open item', () => {
  assert.equal(canAssign(item({ id: 'a', ...twelve })), true);
  assert.equal(canAssign(item({ id: 'a', ...twelve, format: 11 })), false);
  assert.equal(canAssign(item({ id: 'a', ...twelve, accepted: false })), false);
  assert.equal(canAssign(item({ id: 'a', ...twelve, stage: 'closed' })), false);
  assert.equal(canSteer(item({ id: 'a', ...twelve, stage: 'held' })), true);
  assert.equal(canSteer(item({ id: 'a', ...twelve, stage: 'held', format: 11 })), false);
  assert.equal(canSteer(item({ id: 'a', ...twelve, stage: 'ready' })), false);
  assert.equal(canTakeAfterLimit(item({ id: 'a', ...twelve, stage: 'needs_you', needs: 'tries_used' }), 'ana'), true);
  assert.equal(canTakeAfterLimit(item({ id: 'a', ...twelve, stage: 'needs_you', needs: 'question' }), 'ana'), false);
  // Already the viewer's: taking it again would change nothing.
  const hers = item({ id: 'a', ...twelve, stage: 'needs_you', needs: 'tries_used', assignee: { person: 'ana' } });
  assert.equal(canTakeAfterLimit(hers, 'ana'), false);
  assert.equal(canTakeAfterLimit(hers, 'bo'), true);
  // The write itself is refused before sending on a log that is too old.
  const old = item({ id: 'a', ...twelve, format: 11 });
  const [job] = jobsFor(['a'], { kind: 'take' }, [old], NO_WRITES);
  assert.equal(typeof job.req, 'string');
  const [ok] = jobsFor(['a'], { kind: 'take' }, [item({ id: 'a', ...twelve })], NO_WRITES);
  assert.deepEqual(ok.req, { path: '/v1/work/take', body: { item: 'a' } });
  const [lost] = jobsFor(['a'], { kind: 'steer', attempt: 1, text: 'x' }, [item({ id: 'a', ...twelve })], NO_WRITES);
  assert.match(lost.req as string, /not held/);
});

test('the held attempt comes from the fold, else the newest open attempt of the story', () => {
  const held = item({ id: 'a', ...twelve, stage: 'held' });
  const story = { attempts: [{ attempt: 1, done: true, closed: false }, { attempt: 2, done: false, closed: false }] };
  assert.equal(heldAttempt(held, story), 2);
  assert.equal(heldAttempt({ ...held, held_attempt: 5 }, story), 5);
  assert.equal(heldAttempt(held), null);
  assert.equal(heldAttempt(item({ id: 'a', ...twelve, stage: 'ready' }), story), null);
});

test('a steer note must say something and fit', () => {
  assert.equal(checkSteer('  '), 'Write the note for the agent.');
  assert.equal(checkSteer('x'.repeat(5000)), 'Keep the note under 4 KiB, or link to the longer text.');
  assert.equal(checkSteer('use the fixture'), null);
});

test('a taken item says who took it, and an assignee has initials', () => {
  const taken = item({ id: 'a', stage: 'needs_you', needs: 'taken', assignee: { person: 'ana' } });
  assert.equal(needsLine(taken), 'Taken by ana');
  assert.equal(needsLine(item({ id: 'a', stage: 'needs_you', needs: 'question' })), 'An agent asked a question');
  assert.equal(assigneePerson(taken), 'ana');
  assert.equal(assigneePerson(item({ id: 'a', assignee: { agent: 'a1' } })), null);
  assert.equal(initials('ana lopez'), 'AL');
  assert.equal(initials('ana'), 'AN');
});

test('my queue holds what is unassigned or assigned to the signed-in person', async () => {
  const { inScope, myQueue } = await import('../src/list.ts');
  const items = [
    item({ id: 'free', stage: 'needs_you', needs: 'question' }),
    item({ id: 'mine', stage: 'needs_you', needs: 'taken', assignee: { person: 'ana' } }),
    item({ id: 'theirs', stage: 'needs_you', needs: 'taken', assignee: { person: 'bo' } }),
    item({ id: 'agent', stage: 'needs_you', needs: 'question', assignee: { agent: 'a1' } }),
    item({ id: 'ready' }),
  ];
  assert.deepEqual(myQueue(items, 'ana').map((i) => i.id), ['free', 'mine', 'agent']);
  assert.deepEqual(myQueue(items, 'bo').map((i) => i.id), ['free', 'theirs', 'agent']);
  // Signed out, an assigned item is no one's, as in the tracker's queue.
  assert.deepEqual(myQueue(items, null).map((i) => i.id), ['free', 'agent']);
  const signed = { kind: 'signed-in' as const, person: 'ana', reviewLimit: 0 };
  assert.deepEqual(inScope({ items, blocked: [], view: 'inbox', tracker: signed }).map((i) => i.id), ['free', 'mine', 'agent']);
  // The other views are not narrowed by who is signed in.
  assert.equal(inScope({ items, blocked: [], view: 'all', tracker: signed }).length, 5);
});

test('a format refusal on an assign says to raise the log to 12', () => {
  const w: Write = { item: 'a', kind: 'take', batch: 1, status: 'refused', reason: 'raw', why: 'format', id: 1 };
  assert.match(summary([w]).text, /raise the log to format 12 to assign and steer/);
  assert.equal(summary([{ ...w, status: 'ok', why: undefined }]).text, 'Took a.');
});

test('the assign key and command exist', async () => {
  const { commandFor, COMMANDS } = await import('../src/keys.ts');
  assert.equal(commandFor({ key: 'a', metaKey: false, ctrlKey: false, altKey: false, inText: false }, false), 'assign');
  assert.equal(commandFor({ key: 'a', metaKey: false, ctrlKey: false, altKey: false, inText: true }, false), undefined);
  assert.ok(COMMANDS.some((c) => c.id === 'assign'));
});

test('the assign picker writes nothing for the choice the item already has', () => {
  assert.deepEqual(assignPick(null, 'ana', 'me'), { kind: 'take' });
  assert.equal(assignPick('ana', 'ana', 'me'), null);
  assert.equal(assignPick(null, 'ana', 'nobody'), null);
  assert.deepEqual(assignPick('bo', 'ana', 'nobody'), { kind: 'reassign', to: null });
});

test('assigning asks to sign in only for an item that takes an assignee', () => {
  const open = item({ id: 'a', ...twelve });
  const old = item({ id: 'b', ...twelve, format: 11 });
  assert.equal(assignTarget([open, old], 'a', true), open);
  assert.equal(assignTarget([open, old], 'a', false), 'sign-in');
  assert.equal(assignTarget([open, old], 'b', false), null);
  assert.equal(assignTarget([open, old], 'nope', false), null);
});

test('a redo writes the note first, then names it, from the decision’s own attempt', () => {
  const plain = request('a', { kind: 'redo', fromAttempt: 2, at: 31, note: '  ' });
  assert.deepEqual(plain, { path: '/v1/work/redo', body: { item: 'a', from_attempt: 2, at: 31 } });
  const noted = request('a', { kind: 'redo', fromAttempt: 2, at: 31, note: ' use the lock ' });
  assert.deepEqual(noted.body, { item: 'a', from_attempt: 2, at: 31 });
  assert.deepEqual(noted.note, { path: '/v1/work/note', body: { item: 'a', text: { text: 'use the lock' } } });
  assert.equal(route(noted.path, 'ops'), '/v1/teams/ops/work/redo');
});

test('a redo is not offered past its limits', () => {
  const story = { attempts: [{ attempt: 1, closed: false }, { attempt: 2, closed: true }] };
  const open = item({ id: 'a', stage: 'needs_you', redos_left: 2 });
  assert.equal(redoBlock(open, story, 1), null);
  // Budget spent, item closed or done, an agent holding it, a redo waiting, an attempt the story lacks.
  assert.match(redoBlock({ ...open, redos_left: 0 }, story, 1)!, /No redos/);
  assert.match(redoBlock({ ...open, stage: 'closed' }, story, 1)!, /closed/);
  assert.match(redoBlock({ ...open, stage: 'done' }, story, 1)!, /closed/);
  assert.match(redoBlock({ ...open, stage: 'held', held_attempt: 1 }, story, 1)!, /holds/);
  assert.match(redoBlock({ ...open, redo_pending: true }, story, 1)!, /already waits/);
  assert.match(redoBlock(open, story, 9)!, /not in the story/);
  assert.match(redoBlock(open, undefined, 1)!, /not in the story/);
  // The fold allows two open attempts at once.
  assert.match(redoBlock(open, { attempts: [{ attempt: 1, closed: false }, { attempt: 2, closed: false }] }, 1)!, /Two attempts/);
});

test('a red check needs a note on the redo, and a note stays under 4 KiB', () => {
  assert.equal(redoNeedsNote({ pr_phase: 'open', pr_checks: 'red' }), true);
  assert.equal(redoNeedsNote({ pr_phase: 'open', pr_checks: 'green' }), false);
  assert.equal(redoNeedsNote({ pr_phase: 'merged', pr_checks: 'red' }), false);
  assert.match(checkRedo('  ', true)!, /note/);
  assert.equal(checkRedo('', false), null);
  assert.match(checkRedo('x'.repeat(5000), false)!, /4 KiB/);
});

test('the redo dialog says what happens and how many redos remain', () => {
  const d = { offset: 31, attempt: 2, chosen: 'take the lock' } as DecisionLine;
  const c = redoCopy({ id: 'a', redos_left: 2 }, d);
  assert.equal(c.title, 'Redo a from decision 31?');
  assert.match(c.does, /branching from decision 31 on attempt 2/);
  assert.match(c.budget, /2 left now, 1 after this one/);
  assert.match(redoCopy({ id: 'a' }, d).budget, /counts against/);
});

test('the redo key and command exist', async () => {
  const { commandFor, COMMANDS } = await import('../src/keys.ts');
  assert.equal(commandFor({ key: 'r', metaKey: false, ctrlKey: false, altKey: false, inText: false }, false), 'redo');
  assert.equal(commandFor({ key: 'r', metaKey: false, ctrlKey: false, altKey: false, inText: true }, false), undefined);
  assert.ok(COMMANDS.some((c) => c.id === 'redo'));
});

test('a redo confirmed earlier is refused before its note is written when the item moved on', () => {
  const story = { item: 'a', rev: 1, attempts: [{ attempt: 1, closed: false }], groups: [{ phase: 'fix', decisions: [{ offset: 5, attempt: 1 }] }] } as never;
  const ok = item({ id: 'a', stage: 'needs_you', redos_left: 1 });
  const a = { fromAttempt: 1, at: 5, note: 'x' };
  assert.equal(redoStale(ok, story, a), null);
  assert.match(redoStale({ ...ok, stage: 'held', held_attempt: 1 }, story, a)!, /Nothing was sent/);
  assert.match(redoStale({ ...ok, redos_left: 0 }, story, a)!, /No redos/);
  assert.match(redoStale(ok, story, { ...a, fromAttempt: 2 })!, /not in the story/);
  assert.match(redoStale(ok, story, { ...a, at: 9 })!, /decision is not/);
  assert.match(redoStale({ ...ok, pr_phase: 'open', pr_checks: 'red' }, story, { ...a, note: '' })!, /check failed/);
});

test('needs are set on an open root from format 20, through the classify route, and an empty list clears', () => {
  const root = item({ id: 'r', format: 20, class: { needs: { values: ['gpu'], by: 'person' } } });
  const child = item({ id: 'r-f', format: 20, parent: 'r' });
  const old = item({ id: 'o', format: 19 });
  const done = item({ id: 'd', format: 20, stage: 'done' });
  assert.deepEqual([canSetNeeds(root), canSetNeeds(child), canSetNeeds(old), canSetNeeds(done)], [true, false, false, false]);
  assert.deepEqual([logTakesNeeds([old]), logTakesNeeds([old, root])], [false, true]);
  assert.deepEqual(request('r', { kind: 'needs', needs: ['gpu', 'azure'] }), { path: '/v1/work/classify', body: { item: 'r', needs: ['gpu', 'azure'] } });
  assert.deepEqual(request('r', { kind: 'needs', needs: [] }), { path: '/v1/work/classify', body: { item: 'r', needs: [] } });
  const sent = jobsFor(['r', 'r-f'], { kind: 'needs', needs: [] }, [root, child], NO_WRITES);
  assert.equal(typeof sent[0].req, 'object');
  assert.match(String(sent[1].req), /cannot take needs/);
});

test('a needs field is words split on commas, each once, and a repeat of the recorded set sends nothing', () => {
  assert.deepEqual(parseNeeds(' gpu, Azure  gpu,,azure '), ['gpu', 'Azure']);
  assert.equal(checkNeeds(''), null);
  assert.match(checkNeeds('has/ok bad!') ?? '', /bad!/);
  assert.match(checkNeeds('a b c d e f g h i') ?? '', /at most 8/);
  assert.equal(needsPick(['gpu', 'azure'], 'GPU, azure'), null);
  assert.deepEqual(needsPick(['gpu'], ''), { kind: 'needs', needs: [] });
  assert.deepEqual(needsPick([], 'gpu'), { kind: 'needs', needs: ['gpu'] });
});

test('the needs key and command exist and do not fire while typing', async () => {
  const { commandFor, COMMANDS } = await import('../src/keys.ts');
  assert.equal(commandFor({ key: 'n', metaKey: false, ctrlKey: false, altKey: false, inText: false }, false), 'needs');
  assert.equal(commandFor({ key: 'n', metaKey: false, ctrlKey: false, altKey: false, inText: true }, false), undefined);
  assert.ok(COMMANDS.some((c) => c.id === 'needs'));
});
