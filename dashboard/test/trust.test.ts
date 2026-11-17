import assert from 'node:assert/strict';
import test from 'node:test';
import { Tracker, type Fetch } from '../src/tracker.ts';
import { parseTrust, trustText } from '../src/trust.ts';

const stats = (o: Record<string, unknown> = {}) => ({
  days: 30,
  stats: { rated: 12, accurate: 9, not_accurate: 3, accurate_permille: 750, unrated: 4, min_sample: 10, undated: 0, ...o },
});

test('the tracker answer is read into counts and a share', () => {
  assert.deepEqual(parseTrust(stats()), { days: 30, rated: 12, accurate: 9, notAccurate: 3, permille: 750, unrated: 4, minSample: 10, undated: 0 });
  // Nothing rated has no share, which is not zero.
  assert.equal(parseTrust(stats({ rated: 0, accurate: 0, not_accurate: 0, accurate_permille: null }))?.permille, null);
});

test('an answer that is not the numbers is refused, not drawn as zero', () => {
  for (const bad of [null, {}, { days: 30 }, stats({ rated: '12' }), stats({ unrated: -1 }), stats({ accurate_permille: 'x' }), { days: 30, stats: null }]) {
    assert.equal(parseTrust(bad), null, JSON.stringify(bad));
  }
});

test('enough marks show the share with no warning', () => {
  const x = trustText(parseTrust(stats())!);
  assert.equal(x.headline, '9 of 12 summaries marked accurate in the last 30 days');
  assert.equal(x.share, '75% accurate');
  assert.deepEqual([x.small, x.warning], [false, null]);
  assert.equal(x.unrated, '4 finished attempts have no mark yet.');
});

test('a small sample is said so, with the counts first', () => {
  const x = trustText(parseTrust(stats({ rated: 3, accurate: 3, not_accurate: 0, accurate_permille: 1000, unrated: 1 }))!);
  assert.equal(x.headline, '3 of 3 summaries marked accurate in the last 30 days');
  assert.equal(x.share, '100% accurate');
  assert.equal(x.small, true);
  assert.match(x.warning!, /Only 3 marks: too few to read a trend from \(fewer than 10\)/);
  assert.equal(x.unrated, '1 finished attempt has no mark yet.');
  assert.match(trustText(parseTrust(stats({ rated: 1, accurate: 1, not_accurate: 0, accurate_permille: 1000 }))!).headline, /^1 of 1 summary marked/);
});

test('nothing marked, and only unmarked attempts, give no percentage', () => {
  const x = trustText(parseTrust(stats({ rated: 0, accurate: 0, not_accurate: 0, accurate_permille: null, unrated: 7 }))!);
  assert.equal(x.headline, 'No summaries marked in the last 30 days');
  assert.deepEqual([x.share, x.warning, x.small], [null, null, true]);
  assert.equal(x.unrated, '7 finished attempts have no mark yet.');
  const none = trustText(parseTrust(stats({ rated: 0, accurate: 0, not_accurate: 0, accurate_permille: null, unrated: 0 }))!);
  assert.equal(none.unrated, '0 finished attempts have no mark yet.');
});

test('marks the tracker could not place in time are said to be left out', () => {
  assert.equal(trustText(parseTrust(stats({ undated: 2 }))!).undated, '2 marks could not be placed in time and are left out.');
  assert.equal(trustText(parseTrust(stats({ undated: 1 }))!).undated, '1 mark could not be placed in time and is left out.');
  assert.equal(trustText(parseTrust(stats())!).undated, null);
});

test('the tracker reads the numbers from the team route with the viewer token', async () => {
  const seen: { url: string; method: string; auth: string }[] = [];
  const fake: Fetch = async (url, init) => {
    seen.push({ url, method: init.method, auth: init.headers.authorization });
    return { status: 200, json: async () => stats() };
  };
  const got = await new Tracker(fake, 'tok', 'default').summaryTrust(90);
  assert.equal((got as { rated: number }).rated, 12);
  assert.deepEqual(seen, [{ url: '/v1/teams/default/work/metrics/summary-trust?days=90', method: 'GET', auth: 'Bearer tok' }]);
  await new Tracker(fake, 'tok', null).summaryTrust(7);
  assert.equal(seen[1]!.url, '/v1/work/metrics/summary-trust?days=7');
});

test('a refused read keeps the tracker words, and a garbled one is a failure', async () => {
  const refused = await new Tracker(async () => ({ status: 403, json: async () => ({ error: 'persons only' }) }), 'tok', null).summaryTrust(30);
  assert.deepEqual(refused, { status: 'refused', why: 'forbidden', reason: 'persons only', signedOut: true });
  const garbled = await new Tracker(async () => ({ status: 200, json: async () => ({ days: 30 }) }), 'tok', null).summaryTrust(30);
  assert.equal((garbled as { status: string }).status, 'failed');
  const down = await new Tracker(async () => Promise.reject(new Error('Failed to fetch')), 'tok', null).summaryTrust(30);
  assert.equal((down as { status: string }).status, 'failed');
});
