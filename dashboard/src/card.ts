// What a card or row says about an item, as text. No DOM: the board and the list both
// read these, so a card and a row cannot disagree.
import type { ItemRow } from './fold.ts';
import { needsLine } from './writes.ts';

const REPO_REF = /^([\w.-]+)\/([\w.-]+)#(\d+)$/;

/** The upstream reference people say aloud: `GetFelix/felix#1694` is `felix#1694`. Ids with no repo stay as they are. */
export function upstreamRef(i: Pick<ItemRow, 'upstream'>): string | null {
  const up = i.upstream;
  if (!up) return null;
  const m = REPO_REF.exec(up);
  return m ? `${m[2]}#${m[3]}` : up;
}

/** The upstream issue's address, only for a GitHub id that names its repository. */
export function upstreamUrl(i: Pick<ItemRow, 'upstream' | 'source'>): string | null {
  const m = i.upstream ? REPO_REF.exec(i.upstream) : null;
  return m && i.source === 'github' ? `https://github.com/${m[1]}/${m[2]}/issues/${m[3]}` : null;
}

/** What leads a card: the upstream reference, with the kind of work for a child; the KBL id when there is none. */
export function lead(i: ItemRow): string {
  const ref = upstreamRef(i);
  if (!ref) return i.id;
  return i.parent && i.work ? `${ref} ${i.work}` : ref;
}

/** What a screen reader hears for an item: the same lead as its card, the KBL id when it is not in `items`. */
export function nameOf(items: readonly ItemRow[], id: string): string {
  const item = items.find((i) => i.id === id);
  return item ? lead(item) : id;
}

/** Time since `ms`, coarse on purpose: a card is read at a glance. */
export function ago(ms: number, now: number): string {
  const s = Math.max(0, Math.round((now - ms) / 1000));
  if (s < 60) return 'just now';
  const m = Math.round(s / 60);
  if (m < 60) return `${m} min ago`;
  const h = Math.round(m / 60);
  return h < 24 ? `${h} h ago` : `${Math.round(h / 24)} d ago`;
}

/** Who holds the item, on which attempt, and how long it has been quiet. Null when no agent holds it. */
export function workingLine(i: ItemRow, now: number): string | null {
  if (!i.holder || !i.held_attempt) return null;
  const quiet = i.active_ms ? `, last active ${ago(i.active_ms, now)}` : '';
  return `${i.holder} working, attempt ${i.held_attempt}${quiet}`;
}

export interface ChildSummary {
  total: number;
  done: number;
  /** Children a person has to answer for. */
  needsYou: number;
  /** The first child an agent holds. */
  working: ItemRow | null;
  kids: ItemRow[];
}

/** The work under each root, grandchildren included, from every item the log has (not only the ones shown). */
export function childIndex(items: readonly ItemRow[]): Map<string, ItemRow[]> {
  const byId = new Map(items.map((i) => [i.id, i]));
  const by = new Map<string, ItemRow[]>();
  for (const i of items) {
    let top = i;
    for (let hops = 0; top.parent && byId.has(top.parent) && hops < 8; hops++) top = byId.get(top.parent)!;
    if (top !== i) by.set(top.id, [...(by.get(top.id) ?? []), i]);
  }
  return by;
}

export function summarize(kids: readonly ItemRow[]): ChildSummary {
  return {
    total: kids.length,
    done: kids.filter((k) => k.stage === 'done' || k.stage === 'closed').length,
    needsYou: kids.filter((k) => k.stage === 'needs_you').length,
    working: kids.find((k) => k.holder) ?? null,
    kids: [...kids],
  };
}

/** The chip on a root: `2 of 3 done`, plus what in there needs a person. */
export function chipText(c: ChildSummary): string {
  const n = c.total === 1 ? '1 sub-task' : `${c.total} sub-tasks`;
  const extra = c.needsYou ? `, ${c.needsYou} need you` : '';
  return `${n}, ${c.done} done${extra}`;
}

/** Why a card is not moving: what it waits on, the failed check, or the person it waits for. Empty when nothing holds it. */
export function reasonLine(i: ItemRow, waits: string | undefined): string {
  if (waits) return `waits on ${waits}`;
  if (!i.needs) return '';
  if (i.needs === 'check_failed' && i.failing_check) return `Check failed: ${i.failing_check}`;
  return needsLine(i);
}

/** The items this one is related to, as `Related: felix#376, felix#1505`; null when it has none. A relation never blocks, so this is not a reason line. */
export function relatedLine(i: Pick<ItemRow, 'related'>, items: readonly ItemRow[], shown = 3): string | null {
  const ids = i.related ?? [];
  if (!ids.length) return null;
  const names = ids.slice(0, shown).map((id) => nameOf(items, id));
  const more = ids.length > shown ? ` and ${ids.length - shown} more` : '';
  return `Related: ${names.join(', ')}${more}`;
}

/** The pull request as `PR #2300` with what upstream shows: merged, closed, or the checks. */
export function prText(i: ItemRow): string | null {
  if (!i.pr) return null;
  const n = i.pr.split('#').pop();
  const state = i.pr_phase === 'merged' ? 'merged' : i.pr_phase === 'closed' ? 'closed' : i.pr_checks === 'green' ? 'checks pass' : i.pr_checks === 'red' ? 'checks fail' : i.pr_checks === 'pending' ? 'checks running' : '';
  return state ? `PR #${n}, ${state}` : `PR #${n}`;
}

/** What a person blocked the item on, such as `Blocked on azure-vms until 21 Nov: no credit`. Null when it has no block that still holds. */
export function blockText(i: Pick<ItemRow, 'block'>, now: number): string | null {
  const b = i.block;
  if (!b || (b.until_ms != null && b.until_ms <= now)) return null;
  const on = b.resource ? ` on ${b.resource}` : '';
  const until = b.until_ms != null ? ` until ${new Date(b.until_ms).toLocaleDateString(undefined, { day: 'numeric', month: 'short' })}` : '';
  return `Blocked${on}${until}: ${b.reason}`;
}

/** The small properties a card carries: tier, source, pull request, and a block that still holds. The assignee has its own chip. */
export function properties(i: ItemRow, now?: number): string[] {
  const tier = i.tier ?? i.proposed_tier;
  const block = now != null ? blockText(i, now) : null;
  return [tier != null ? `Tier ${tier}` : null, i.source ?? null, prText(i), block].filter((x): x is string => !!x);
}
