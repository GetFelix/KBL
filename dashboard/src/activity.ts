// What happened on an item, read from records the fold already applied, and the sentences the
// panel builds from it: the Now line, the timeline and the attempt history. No DOM.
import type { BlockedRow, ItemRow } from './fold.ts';
import { stageLabel } from './list.ts';
import { assigneePerson, needsLine } from './writes.ts';
import { prText } from './card.ts';

export interface ActivityEvent {
  offset: number;
  at_ms: number;
  by: string;
  kind: string;
  attempt?: number;
  /** What the fold charged the item for this record, in micro-dollars. */
  cost_micros?: number;
  text?: string | null;
  command?: string;
  exit_code?: number;
  /** The tail of a step's output; null when it lives in an artifact. */
  output?: string | null;
  file?: string;
  chosen?: string | null;
  why?: string | null;
  reason?: string | null;
  phase?: string;
  options?: string[];
  question?: number;
  accurate?: boolean;
  tier?: number;
  deadline_ms?: number;
  steered?: boolean;
  to?: { person: string } | { agent: string } | null;
  /** Why the fold refused the record (`bad_usage`, ...). A refused record changed nothing. */
  refused?: string;
}

/** The events the fold took. Everything the panel counts, sums or states as fact reads these. */
export function accepted(events: readonly ActivityEvent[]): ActivityEvent[] {
  return events.filter((e) => !e.refused);
}

export type Activities = ReadonlyMap<string, readonly ActivityEvent[]>;

/** The fold's events for each item in `ids`. */
export function activities(fold: { activityJson(item: string): string }, ids: readonly string[]): Map<string, ActivityEvent[]> {
  return new Map(ids.map((id) => [id, JSON.parse(fold.activityJson(id)) as ActivityEvent[]]));
}

/** `40 s ago`, `3 min ago`, or `in 50 s` for a time ahead. Exact enough to judge whether an agent is stuck. */
export function since(ms: number, now: number): string {
  const diff = Math.round((now - ms) / 1000);
  const s = Math.abs(diff);
  const span = s < 60 ? `${s} s` : s < 3600 ? `${Math.round(s / 60)} min` : s < 86400 ? `${Math.round(s / 3600)} h` : `${Math.round(s / 86400)} d`;
  if (s < 5 && diff >= 0) return 'just now';
  return diff >= 0 ? `${span} ago` : `in ${span}`;
}

/** The newest event of an attempt that the agent itself wrote. */
function lastWrite(events: readonly ActivityEvent[], attempt: number): ActivityEvent | undefined {
  return accepted(events).reverse().find((e) => e.attempt === attempt && !['note', 'rated', 'attempt_closed'].includes(e.kind));
}

export interface NowLine {
  text: string;
  /** `warn` when a person has to act or the agent looks stuck. */
  tone: 'ok' | 'warn' | 'plain';
}

/** What an item waits on besides a person: the items it is blocked by, or its pull request's checks. */
export function waitsText(item: ItemRow, blocked: readonly BlockedRow[]): string | null {
  const row = blocked.find((b) => b.id === item.id);
  if (row && row.waits_on.length) return `waiting on ${row.waits_on.map((b) => b.id).join(', ')}`;
  if (item.pr_checks === 'pending') return `waiting on the checks of ${prText(item)?.split(',')[0] ?? 'its pull request'}`;
  return null;
}

/**
 * One plain sentence for the top of the panel: who holds the item and how recently it moved, or
 * why it is not moving and who must act. `now` is the page clock; the fold's times are the tracker's.
 */
export function nowLine(item: ItemRow, blocked: readonly BlockedRow[], events: readonly ActivityEvent[], now: number): NowLine {
  // Before format 14 a question leaves the hold in place, so a held item can wait on a person.
  if (item.holder && item.held_attempt && item.stage !== 'needs_you') {
    const last = item.active_ms ?? lastWrite(events, item.held_attempt)?.at_ms ?? null;
    const quiet = last === null ? 'no activity recorded yet' : `last activity ${since(last, now)}`;
    // Renewals are not recorded, so a passed claim-time deadline says nothing about a live agent.
    const deadline = item.hold_deadline_ms;
    const lapse = deadline != null && deadline > now ? `. Claimed with a deadline ${since(deadline, now)}` : '';
    return { text: `${item.holder} is working on this, attempt ${item.held_attempt}, ${quiet}${lapse}.`, tone: 'ok' };
  }
  const waits = waitsText(item, blocked);
  switch (item.stage) {
    case 'needs_you': {
      const why = needsLine(item).replace(/\.$/, '');
      return { text: `Waiting for a person: ${why.charAt(0).toLowerCase()}${why.slice(1)}.`, tone: 'warn' };
    }
    case 'blocked':
      return { text: `Blocked${waits ? `, ${waits}` : ''}.`, tone: 'warn' };
    case 'ready': {
      const who = assigneePerson(item);
      return { text: who ? `Assigned to ${who}; no agent will take it.` : 'Ready. No agent has claimed it yet.', tone: 'plain' };
    }
    case 'in_review':
      return { text: `Finished, in review${waits ? `, ${waits}` : ''}. A person decides next.`, tone: 'plain' };
    case 'done':
    case 'closed':
      return { text: `${stageLabel(item.stage)}.`, tone: 'plain' };
    default:
      return { text: `${stageLabel(item.stage)}${waits ? `, ${waits}` : ''}.`, tone: 'plain' };
  }
}

export interface TimelineEntry {
  offset: number;
  at_ms: number;
  /** Headline: what happened, in a few words. */
  title: string;
  /** Who did it, with the attempt when there is one. */
  who: string;
  /** Recorded text that goes with it, such as a note or a release reason. */
  detail: string | null;
  /** A step's command output tail, shown folded. */
  output: string | null;
  failed: boolean;
  /** Why the fold refused the record, or null. A refused entry changed nothing. */
  refused: string | null;
  /** The attempt the entry belongs to, to jump to its story. */
  attempt: number | null;
}

/** Plain words for the refusals a person is likely to meet; any other shows its code. */
const REFUSED: Record<string, string> = {
  bad_usage: 'the usage it reported is not allowed',
  not_holder: 'the writer does not hold the item',
  wrong_attempt: 'it names an attempt that is not the held one',
  actor_not_allowed: 'the writer may not do this',
  closed: 'the item is closed',
  not_ready: 'the item is not ready for it',
};

const WHY: Record<string, string> = { failed: 'it failed', waits_on_person: 'it waits on a person', waits_on_outside: 'it waits on something outside' };

function describe(e: ActivityEvent): Pick<TimelineEntry, 'title' | 'detail' | 'output' | 'failed'> {
  const base = { detail: null as string | null, output: null as string | null, failed: false };
  switch (e.kind) {
    case 'claimed': return { ...base, title: 'Claimed' };
    case 'progress': return { ...base, title: 'Progress note', detail: e.text ?? null };
    case 'step': return { ...base, title: `Ran ${e.command ?? 'a command'}`, detail: `exit ${e.exit_code ?? '?'}`, output: e.output ?? null, failed: (e.exit_code ?? 0) !== 0 };
    case 'edit': return { ...base, title: `Edited ${e.file ?? 'a file'}` };
    case 'decision': return { ...base, title: `Decided: ${e.chosen ?? ''}`, detail: [e.phase, e.why].filter(Boolean).join(': ') || null };
    case 'question': return { ...base, title: 'Asked a question', detail: e.text ?? null };
    case 'answer': return { ...base, title: 'Answered', detail: e.chosen ?? e.text ?? null };
    case 'note': return { ...base, title: e.steered ? 'Steered' : 'Note', detail: e.text ?? null };
    case 'done': return { ...base, title: 'Finished the attempt' };
    case 'released': return { ...base, title: 'Released the item', detail: [e.why ? WHY[e.why] ?? e.why : 'it failed', e.reason].filter(Boolean).join(': '), failed: !e.why || e.why === 'failed' };
    case 'gave_up': return { ...base, title: 'Gave up', detail: e.reason ?? null, failed: true };
    case 'attempt_closed': return { ...base, title: 'Attempt closed by a person' };
    case 'rated': return { ...base, title: e.accurate ? 'Summary marked accurate' : 'Summary marked not accurate', failed: e.accurate === false };
    case 'accepted': return { ...base, title: e.tier == null ? 'Accepted' : `Accepted at tier ${e.tier}` };
    case 'tier': return { ...base, title: `Tier set to ${e.tier}` };
    case 'taken': return { ...base, title: 'Taken' };
    case 'reassigned': return { ...base, title: e.to ? `Assigned to ${'person' in e.to ? e.to.person : e.to.agent}` : 'Unassigned' };
    case 'closed': return { ...base, title: 'Closed', detail: e.reason ?? e.why ?? null };
    case 'reopened': return { ...base, title: 'Reopened' };
    default: return { ...base, title: e.kind.replaceAll('_', ' ') };
  }
}

/** The item's events newest first, as the timeline shows them. */
export function timeline(events: readonly ActivityEvent[]): TimelineEntry[] {
  return [...events].reverse().map((e) => {
    const d = describe(e);
    const who = e.attempt != null ? `${e.by}, attempt ${e.attempt}` : e.by;
    if (e.refused) {
      const why = REFUSED[e.refused] ?? e.refused.replaceAll('_', ' ');
      return { offset: e.offset, at_ms: e.at_ms, who, attempt: e.attempt ?? null, title: `Refused: ${d.title}`, detail: [`Refused, ${why}. It changed nothing`, d.detail].filter(Boolean).join('. '), output: null, failed: true, refused: e.refused };
    }
    return { offset: e.offset, at_ms: e.at_ms, who, attempt: e.attempt ?? null, ...d, refused: null };
  });
}

export interface AttemptHistory {
  attempt: number;
  by: string;
  started_ms: number;
  /** `Working`, `Finished`, `Released: ...`, `Gave up: ...`, `Closed by a person`, or `Waiting on a person`. */
  outcome: string;
  /** The last progress note of the attempt, which is what it handed off. */
  summary: string | null;
  /** A person's latest mark of the summary, or null if nobody marked it. */
  accurate: boolean | null;
  cost_micros: number;
  steps: number;
  failedSteps: number;
}

/** One row per attempt, oldest first. A held attempt shows as working. */
export function history(events: readonly ActivityEvent[], item: Pick<ItemRow, 'holder' | 'held_attempt' | 'stage'>): AttemptHistory[] {
  const by = new Map<number, AttemptHistory>();
  for (const e of accepted(events)) {
    if (e.attempt == null) continue;
    let a = by.get(e.attempt);
    if (!a) {
      a = { attempt: e.attempt, by: e.by, started_ms: e.at_ms, outcome: 'Working', summary: null, accurate: null, cost_micros: 0, steps: 0, failedSteps: 0 };
      by.set(e.attempt, a);
    }
    a.cost_micros += e.cost_micros ?? 0;
    if (e.kind === 'claimed') { a.by = e.by; a.started_ms = e.at_ms; }
    else if (e.kind === 'progress' && e.text) a.summary = e.text;
    else if (e.kind === 'step') { a.steps++; if ((e.exit_code ?? 0) !== 0) a.failedSteps++; }
    else if (e.kind === 'done') a.outcome = 'Finished';
    else if (e.kind === 'released') a.outcome = `Released: ${[e.why ? WHY[e.why] ?? e.why : 'it failed', e.reason].filter(Boolean).join(', ')}`;
    else if (e.kind === 'gave_up') a.outcome = `Gave up: ${e.reason ?? ''}`.trim();
    else if (e.kind === 'attempt_closed') a.outcome = 'Closed by a person';
    else if (e.kind === 'question') a.outcome = 'Waiting on a person';
    else if (e.kind === 'rated') a.accurate = e.accurate ?? null;
  }
  const held = item.holder ? item.held_attempt : null;
  // A hold that outlives a question still waits on the person while the item is in Needs you.
  const waiting = item.stage === 'needs_you';
  const heldNow = (a: AttemptHistory): AttemptHistory =>
    a.attempt !== held ? a : waiting && a.outcome === 'Working' ? { ...a, outcome: 'Waiting on a person' } : !waiting && a.outcome === 'Waiting on a person' ? { ...a, outcome: 'Working' } : a;
  return [...by.values()].map(heldNow).sort((x, y) => x.attempt - y.attempt);
}

/** Micro-dollars as `$1.25`, or `under $0.01` for a charge that rounds to nothing. */
export function money(micros: number): string {
  if (micros <= 0) return '$0.00';
  return micros < 5_000 ? 'under $0.01' : `$${(micros / 1_000_000).toFixed(2)}`;
}

export interface Linked {
  /** Children (triage, augment, review, ...) with the answer each recorded, when it has one. */
  children: { id: string; work: string; title: string; stage: string; answer: string | null }[];
  waitsOn: { id: string; title: string | null }[];
  /** Items held back by this one. */
  blocks: { id: string; title: string }[];
}

/** The work linked to `item`. `answers` is each child's latest recorded answer or progress text. */
export function linked(item: ItemRow, items: readonly ItemRow[], blocked: readonly BlockedRow[], acts: Activities): Linked {
  const answerOf = (id: string): string | null => {
    const ev = accepted(acts.get(id) ?? []);
    const a = [...ev].reverse().find((e) => e.kind === 'answer' && (e.chosen || e.text));
    return a ? (a.chosen ?? a.text ?? null) : null;
  };
  return {
    children: items.filter((i) => i.parent === item.id).map((i) => ({ id: i.id, work: i.work ?? 'work', title: i.title, stage: i.stage, answer: answerOf(i.id) })),
    waitsOn: blocked.find((b) => b.id === item.id)?.waits_on.map((b) => ({ id: b.id, title: b.title })) ?? [],
    blocks: blocked.filter((b) => b.waits_on.some((w) => w.id === item.id)).map((b) => ({ id: b.id, title: b.title })),
  };
}
