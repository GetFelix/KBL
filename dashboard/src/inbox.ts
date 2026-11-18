// What waits on a person, sorted oldest first and sorted into kinds. Pure: the list view
// draws it and main.ts sends the writes. The stage and the reason come from the fold.
import type { ItemRow } from './fold.ts';
import { assigneePerson } from './writes.ts';

export const INBOX_KINDS = [
  ['question', 'Questions'],
  ['triage', 'Triage'],
  ['review', 'Review findings'],
  ['decision', 'Decisions'],
] as const;

export type InboxKind = (typeof INBOX_KINDS)[number][0];

export const kindLabel = (k: InboxKind): string => INBOX_KINDS.find(([x]) => x === k)![1];

/**
 * What the item waits for, or null when it waits on nobody. A question is an agent's or triage's
 * open question; triage and review are proposals to accept or reject; a decision is any other
 * reason the fold puts an item in Needs you (blocked, over budget, a failed check).
 */
export function inboxKind(i: ItemRow): InboxKind | null {
  if (i.needs === 'question' || i.needs === 'unclear') return 'question';
  if (i.needs) return 'decision';
  if (i.stage === 'proposed' || (i.stage === 'triage' && i.proposed_tier != null)) return i.finding != null ? 'review' : 'triage';
  return null;
}

/** Everything waiting on `viewer`. An item assigned to another person is theirs, as in the tracker's queue. */
export function inboxFor(items: readonly ItemRow[], viewer: string | null, kind: InboxKind | null = null): ItemRow[] {
  return items.filter((i) => {
    const k = inboxKind(i);
    if (k === null || (kind !== null && k !== kind)) return false;
    const who = assigneePerson(i);
    return who === null || who === viewer;
  });
}

export function kindCounts(rows: readonly ItemRow[]): Record<InboxKind, number> {
  const counts: Record<InboxKind, number> = { question: 0, triage: 0, review: 0, decision: 0 };
  for (const i of rows) {
    const k = inboxKind(i);
    if (k) counts[k]++;
  }
  return counts;
}

/** The log offset at which an item began waiting for what it waits for now. */
export interface Waiting {
  key: string;
  at: number;
}

/**
 * Carries each item's wait forward: it keeps its offset while the reason stays the same, and
 * starts again at `at` when the reason changes. A question's own offset is exact and wins.
 */
export function trackWaiting(prev: ReadonlyMap<string, Waiting>, items: readonly ItemRow[], at: number): Map<string, Waiting> {
  const next = new Map<string, Waiting>();
  for (const i of items) {
    const k = inboxKind(i);
    if (k === null) continue;
    const key = `${k}|${i.needs ?? i.stage}|${i.question ?? ''}`;
    const old = prev.get(i.id);
    next.set(i.id, old && old.key === key ? old : { key, at: i.question ?? at });
  }
  return next;
}

/** Oldest first; ties and items never seen waiting fall back to the id, so the order never flickers. */
export function oldestFirst(rows: readonly ItemRow[], waiting: ReadonlyMap<string, Waiting>): ItemRow[] {
  const at = (i: ItemRow) => i.question ?? waiting.get(i.id)?.at ?? Number.MAX_SAFE_INTEGER;
  return [...rows].sort((a, b) => at(a) - at(b) || a.id.localeCompare(b.id));
}

/** The option a digit key picks: 1 is the first option. */
export const optionForKey = (options: readonly string[], key: string): string | null => (/^[1-9]$/.test(key) ? (options[Number(key) - 1] ?? null) : null);
