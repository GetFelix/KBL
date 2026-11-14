// A small work.events log with items in most stages, for the browser check and
// the design screenshots. Record shapes follow wasm/golden/every_record.jsonl.
// GitHub ids name their repo (`owner/repo#n`), which is how the fold records one; the
// others record a source only. The tier is what triage and acceptance recorded.
const ISSUES = [
  ['gh-2204', 'Reads miss a fresh write after a restart', 'needs_you', 'GetFelix/felix', 1],
  ['gh-2210', 'Consumer group rebalance drops the last ack', 'in_review', 'GetFelix/felix', 2],
  ['gh-2215', 'Publish stalls when a follower lags by one segment', 'held', 'GetFelix/felix', 1],
  ['gh-2219', 'Retention deletes a segment a reader still holds', 'ready', 'GetFelix/felix', 1],
  ['gh-2221', 'Gateway sends a stale offset after reconnect', 'ready', 'GetFelix/felix-gateway', 2],
  ['ado-4377', 'Snapshot install races with a leader change', 'triage'],
  ['ado-4380', 'Metrics endpoint reports negative lag', 'triage'],
  ['jira-214', 'Document the fsync modes for operators', 'done', null, 2],
  ['gh-2190', 'Flaky cluster test: lease expiry under load', 'done', 'GetFelix/felix', 1],
];

export function fixtureLog() {
  const log = [];
  let at = 1_700_000_000_000;
  const rec = (r) => log.push({ actor: 'person', by: 'maintainer', at_ms: at++, ...r });
  // Claims, questions and finished work come from an agent; the fold refuses them from a person.
  const agent = (r) => rec({ actor: 'agent', by: 'agent-1', ...r });
  for (const [item, title, stage, repo, tier] of ISSUES) {
    const [source, number] = item.split('-');
    const upstream_id = repo ? `${repo}#${number}` : item;
    rec({ kind: 'imported', item, source: { gh: 'github', ado: 'ado', jira: 'jira' }[source], title, upstream_id });
    if (stage === 'triage') continue;
    rec({ kind: 'triaged', item, tier, why: 'seen in the field' });
    rec({ kind: 'accepted', item, tier });
    if (stage === 'ready') continue;
    agent({ kind: 'claimed', item, attempt: 1, deadline_ms: at + 3_600_000 });
    if (stage === 'held') continue;
    if (stage === 'needs_you') {
      agent({ kind: 'question', item, attempt: 1, text: { text: 'Which client saw it?' } });
      continue;
    }
    agent({ kind: 'done', item, attempt: 1, cost_micros: 1_250_000 });
    if (stage === 'done') rec({ kind: 'closed', item, why: 'merged' });
  }
  // Blocked work: one item waits on a gateway issue in another repo and on a Jira issue,
  // another on the release of a merged fix.
  const imp = (item, source, upstream_id, title) => rec({ kind: 'imported', item, source, upstream_id, title });
  imp('blk-40', 'github', 'GetFelix/felix-blocks#40', 'Blocks need compare-and-set writes');
  imp('gw-31', 'github', 'GetFelix/felix-gateway#31', 'Compare-and-set on the gateway');
  imp('jira-300', 'jira', 'site/FEL-300', 'Agree the write semantics');
  imp('blk-41', 'github', 'GetFelix/felix-blocks#41', 'Ship the undo button');
  imp('blk-42', 'github', 'GetFelix/felix-blocks#42', 'Undo needs the history fix');
  rec({ kind: 'accepted', item: 'blk-40', tier: 1 });
  rec({ kind: 'linked', item: 'blk-40', to: 'gw-31' });
  rec({ kind: 'linked', item: 'blk-40', to: 'jira-300' });
  rec({ kind: 'accepted', item: 'blk-41', tier: 1 });
  agent({ kind: 'claimed', item: 'blk-41', attempt: 1, deadline_ms: at + 3_600_000 });
  agent({ kind: 'prerequisite', item: 'blk-41', attempt: 1, prerequisite: 'blk-42', key: 'history', done_when: 'released', cost_micros: 0 });
  rec({ kind: 'closed', item: 'blk-42', why: 'merged' });
  // Two items in one repo that name the same files, so the overlap note and flags show.
  rec({ kind: 'context', item: 'gh-2219', entry: 'files', text: { text: 'core/src/retention.rs\ncore/src/segment/' } });
  rec({ kind: 'context', item: 'gh-2215', entry: 'files', text: { text: 'core/src/segment/reader.rs\ncore/src/publish.rs' } });
  return log;
}
