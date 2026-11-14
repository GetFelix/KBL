// A small work.events log with items in most stages, for the browser check and
// the design screenshots. Record shapes follow wasm/golden/every_record.jsonl.
const ISSUES = [
  ['gh-2204', 'Reads miss a fresh write after a restart', 'needs_you'],
  ['gh-2210', 'Consumer group rebalance drops the last ack', 'in_review'],
  ['gh-2215', 'Publish stalls when a follower lags by one segment', 'held'],
  ['gh-2219', 'Retention deletes a segment a reader still holds', 'ready'],
  ['gh-2221', 'Gateway sends a stale offset after reconnect', 'ready'],
  ['ado-4377', 'Snapshot install races with a leader change', 'triage'],
  ['ado-4380', 'Metrics endpoint reports negative lag', 'triage'],
  ['jira-214', 'Document the fsync modes for operators', 'done'],
  ['gh-2190', 'Flaky cluster test: lease expiry under load', 'done'],
];

export function fixtureLog() {
  const log = [];
  let at = 1_700_000_000_000;
  const rec = (r) => log.push({ actor: 'person', by: 'maintainer', at_ms: at++, ...r });
  // Claims, questions and finished work come from an agent; the fold refuses them from a person.
  const agent = (r) => rec({ actor: 'agent', by: 'agent-1', ...r });
  for (const [item, title, stage] of ISSUES) {
    const [source] = item.split('-');
    rec({ kind: 'imported', item, source: { gh: 'github', ado: 'ado', jira: 'jira' }[source], title, upstream_id: item });
    if (stage === 'triage') continue;
    rec({ kind: 'triaged', item, tier: 1, why: 'seen in the field' });
    rec({ kind: 'accepted', item, tier: 1 });
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
  return log;
}
