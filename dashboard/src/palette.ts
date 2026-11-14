// What the command palette lists and in what order. Pure, so it is tested without a page.

export interface Entry {
  /** Unique within one palette list. */
  key: string;
  label: string;
  /** A second line of context, e.g. an item's stage. */
  hint?: string;
  section: string;
}

/**
 * Scores `label` for `query`: higher is better, -1 is no match. A substring beats a
 * scattered match, and a match at a word start beats one inside a word, so "rea" finds
 * "Ready" before "thread". Case and extra spaces are ignored.
 */
export function score(label: string, query: string): number {
  const q = query.trim().toLowerCase().replace(/\s+/g, ' ');
  if (!q) return 0;
  const l = label.toLowerCase();
  const at = l.indexOf(q);
  if (at >= 0) return (at === 0 || l[at - 1] === ' ' ? 300 : 200) - at;
  // Every query character in order, spaces skipped: forgiving of a dropped letter.
  let i = 0;
  for (const ch of l) if (ch === q[i] || q[i] === ' ') i++;
  return i >= q.length ? 100 - l.length : -1;
}

/** The entries that match `query`, best first; ties keep their given order. */
export function rank(entries: readonly Entry[], query: string): Entry[] {
  return entries
    .map((e, i) => ({ e, i, s: Math.max(score(e.label, query), score(e.key, query)) }))
    .filter((x) => x.s >= 0)
    .sort((a, b) => b.s - a.s || a.i - b.i)
    .map((x) => x.e);
}
