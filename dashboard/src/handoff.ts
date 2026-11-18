// What a finished attempt handed off, read from the fold (wasm/src/activity.rs): the agent's
// last note, the lines its edits touch, its failed steps and the reviewer's mark, beside the
// pull request's recorded checks. Only recorded fields; a missing one is left out. No DOM.
import type { Fold, ItemRow } from './fold.ts';

export interface FailedStep {
  offset: number;
  command: string;
  exit_code: number;
}

export interface Handoff {
  /** The latest attempt the fold saw finish. */
  attempt: number;
  summary: string | null;
  files: number;
  added: number;
  removed: number;
  /** A hunk is stored outside the log, so the line totals are a floor. */
  partial: boolean;
  failed_steps: FailedStep[];
  failed_total: number;
  /** The reviewer's standing mark, or null if nobody marked the summary. */
  accurate: boolean | null;
}

export type Handoffs = ReadonlyMap<string, Handoff>;

export function handoffs(fold: Pick<Fold, 'handoffsJson'>): Map<string, Handoff> {
  return new Map(Object.entries(JSON.parse(fold.handoffsJson()) as Record<string, Handoff>));
}

/** The pull request's address, only for an `owner/repo#number` reference. */
export function prUrl(pr: string | null | undefined): string | null {
  const m = pr ? /^([\w.-]+)\/([\w.-]+)#(\d+)$/.exec(pr) : null;
  return m ? `https://github.com/${m[1]}/${m[2]}/pull/${m[3]}` : null;
}

const count = (n: number, one: string, many: string) => `${n.toLocaleString('en-US')} ${n === 1 ? one : many}`;

/** `3 files, +42 -7`, with a note when part of the diff is not in the log. */
export function diffText(h: Handoff): string {
  if (h.files === 0 && h.added === 0 && h.removed === 0 && !h.partial) return 'No edits recorded';
  const lines = `+${h.added} -${h.removed}`;
  return `${count(h.files, 'file', 'files')}, ${h.partial ? `at least ${lines} (some hunks are stored outside the log)` : lines}`;
}

/** The recorded checks of the item's pull request, or null when it reports none. */
export function checksText(item: Pick<ItemRow, 'pr' | 'pr_phase' | 'pr_checks' | 'failing_check'>): string | null {
  if (!item.pr) return null;
  switch (item.pr_checks) {
    case 'green': return 'Checks pass';
    case 'red': return item.failing_check ? `Checks fail: ${item.failing_check}` : 'Checks fail';
    case 'pending': return 'Checks running';
    default: return null;
  }
}

export function failedText(h: Handoff): string | null {
  if (h.failed_total === 0) return null;
  const more = h.failed_total - h.failed_steps.length;
  return `${count(h.failed_total, 'step failed', 'steps failed')}${more > 0 ? ` (${more} not listed)` : ''}`;
}

export interface Mark {
  accurate: boolean;
  label: string;
  pressed: boolean;
}

/** The two marks a reviewer can give; the recorded one is pressed. */
export const marks = (h: Pick<Handoff, 'accurate'>): Mark[] => [
  { accurate: true, label: 'Accurate', pressed: h.accurate === true },
  { accurate: false, label: 'Not accurate', pressed: h.accurate === false },
];

export const markText = (h: Pick<Handoff, 'accurate'>): string =>
  h.accurate === null ? 'Summary not marked yet' : h.accurate ? 'Summary marked accurate' : 'Summary marked not accurate';

/** Changes whenever anything the block shows changes, so a cached block is rebuilt. */
export const handoffKey = (item: ItemRow, h: Handoff | undefined): string => (h ? JSON.stringify([item.id, h, item.pr, item.pr_checks, item.failing_check]) : '');
