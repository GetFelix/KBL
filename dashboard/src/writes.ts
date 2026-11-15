// What a person can do to an item, the tracker request for each, how its answer reads,
// and which writes can be undone. No DOM: main.ts sends the requests and the views show
// the results. Every write is one of the tracker's person routes, sent with the viewer's
// own token, so the page can never do more than the person could with curl.
import type { ItemRow } from './fold.ts';

export type WriteKind = 'accept' | 'reject' | 'tier' | 'close' | 'answer' | 'undo';

export interface Request {
  path: string;
  body: Record<string, unknown>;
}

/** Why an item is in Needs you, in words, from the fold's `needs`. */
export const NEEDS: Readonly<Record<string, string>> = {
  question: 'An agent asked a question',
  unclear: 'Triage could not settle what it is',
  over_budget: 'Over its budget',
  dependency_dropped: 'Something it waits on was dropped',
  tries_used: 'Its attempt used its tries',
  no_open_attempt: 'No attempt is open',
  left_scope: 'The issue left its intake rule',
  check_failed: 'A check failed',
  flagged: 'A person moved it here',
};

export function needsText(needs: string | null | undefined): string {
  return needs ? (NEEDS[needs] ?? needs.replaceAll('_', ' ')) : '';
}

/** What triage could not settle, as the answer form asks it. Its question has no text of its own. */
export const UNCLEAR: Readonly<Record<string, string>> = {
  scope: 'Triage could not tell what this item asks for. What should the work be?',
  repo: 'Triage could not tell which repository the work belongs in. Which one is it?',
  acceptance: 'Triage could not tell what would count as done. What should be true when it is?',
};

/** The tier the panel and an accept start from: the recorded one, else triage's proposal, else none. */
export const shownTier = (i: ItemRow): number | null => i.tier ?? i.proposed_tier ?? null;

/**
 * The tier an accept sends. `picked` is the tier field's value ('' for none shown or "keep
 * each"). A tier the person saw is always sent, so a proposal triage changes while the
 * confirm is open cannot apply a tier they never saw; none is sent only when none was shown.
 */
export function acceptTier(picked: string): number | null {
  return picked === '' ? null : Number(picked);
}

const OPEN = (i: ItemRow) => i.stage !== 'done' && i.stage !== 'closed';

/** Which writes the fold would take for this item. The tracker still decides; this only hides what it would refuse. */
export const CAN: Readonly<Record<Exclude<WriteKind, 'undo'>, (i: ItemRow) => boolean>> = {
  accept: (i) => i.stage === 'triage' || i.stage === 'proposed',
  // The fold rejects only a proposal nobody accepted; an imported root is closed instead.
  reject: (i) => i.stage === 'proposed',
  tier: OPEN,
  close: OPEN,
  answer: (i) => (i.needs === 'question' || i.needs === 'unclear') && i.question != null,
};

export const CLOSE_WHY = [
  ['dismissed', 'Dismissed: not going to be done'],
  ['completed', 'Completed: done outside KBL'],
] as const;
export type CloseWhy = (typeof CLOSE_WHY)[number][0];

export type WriteArgs =
  | { kind: 'accept'; tier: number | null }
  | { kind: 'reject' }
  | { kind: 'tier'; tier: number }
  | { kind: 'close'; why: CloseWhy }
  | { kind: 'answer'; question: number; text: string; chosen: string | null };

/** A route under the viewer's team when the address names one (`/v1/teams/{team}/work/...`). */
export function route(path: string, team: string | null): string {
  return team ? `/v1/teams/${encodeURIComponent(team)}/${path.slice('/v1/'.length)}` : path;
}

export function request(item: string, a: WriteArgs): Request {
  switch (a.kind) {
    case 'accept':
      return { path: '/v1/work/proposals/accept', body: a.tier === null ? { item } : { item, tier: a.tier } };
    case 'reject':
      return { path: '/v1/work/proposals/reject', body: { item } };
    case 'tier':
      return { path: '/v1/work/tier', body: { item, tier: a.tier } };
    case 'close':
      return { path: '/v1/work/close', body: { item, why: a.why } };
    case 'answer': {
      const body: Record<string, unknown> = { item, question: a.question };
      // The tracker reads `text` as content: inline text is `{ text }`, never a bare string.
      if (a.text.trim()) body.text = { text: a.text.trim() };
      if (a.chosen !== null) body.chosen = a.chosen;
      return { path: '/v1/work/questions/answer', body };
    }
  }
}

/** What an answer form would send, or what is wrong with it (WCAG 3.3.1, 3.3.3). */
export function checkAnswer(options: readonly string[], text: string, chosen: string | null): string | null {
  if (options.length > 0 && chosen === null && !text.trim()) return 'Pick an option, or write an answer.';
  // The tracker takes one or the other, never both.
  if (chosen !== null && text.trim()) return 'Pick an option or write an answer, not both. Clear the text to send the option.';
  if (options.length === 0 && !text.trim()) return 'Write an answer.';
  if (chosen !== null && !options.includes(chosen)) return 'That option is no longer offered.';
  // The tracker refuses inline text over 4 KiB; say so before sending.
  if (new TextEncoder().encode(text.trim()).length > 4096) return 'Keep the answer under 4 KiB, or link to the longer text.';
  return null;
}

export type Outcome =
  | { status: 'ok'; offset: number | null }
  | { status: 'refused'; why: string; reason: string; signedOut?: true }
  | { status: 'failed'; reason: string };

/**
 * Reads the tracker's answer. A refusal keeps the tracker's own words and code, so the
 * page shows its reason rather than a guess. `status` is null when the request never got
 * an answer.
 */
export function outcome(status: number | null, body: unknown, network = ''): Outcome {
  const b = (body && typeof body === 'object' ? body : {}) as { error?: unknown; why?: unknown; offset?: unknown };
  const error = typeof b.error === 'string' ? b.error : '';
  const why = typeof b.why === 'string' ? b.why : '';
  if (status === null) return { status: 'failed', reason: `Cannot reach the tracker${network ? `: ${network}` : ''}.` };
  if (status >= 200 && status < 300) return { status: 'ok', offset: typeof b.offset === 'number' ? b.offset : null };
  // Only the status says the token itself was refused; a `why` in a 409 is the fold's.
  if (status === 401) return { status: 'refused', why: why || 'signed_out', reason: error || 'The tracker does not know this token. Sign in again.', signedOut: true };
  if (status === 403) return { status: 'refused', why: why || 'forbidden', reason: error || 'This token may not make that write.', signedOut: true };
  if (status === 400 || status === 404 || status === 409 || status === 422) {
    return { status: 'refused', why: why || String(status), reason: error || `The tracker refused it (${status}).` };
  }
  if (status === 503) return { status: 'failed', reason: `${error || 'The tracker is busy'}. Nothing was written; try again.` };
  return { status: 'failed', reason: `${error || `The tracker answered ${status}`}. It may not have been written; the list shows what the log holds.` };
}

/**
 * How to undo a write. Only a tier change can be: setting the old tier back is a new
 * `tier_changed` record, and the log keeps both. The others have no record that takes
 * them back, so the page asks before sending them.
 */
export const UNDO: Readonly<Record<Exclude<WriteKind, 'undo'>, string | null>> = {
  tier: null,
  accept: 'No record takes back an acceptance, and an accepted item is published for agents at once.',
  reject: 'A rejected proposal stays rejected; nothing reopens it.',
  close: 'Closing cancels the item’s open children. A root can be reopened, but it goes back to Triage and its children stay cancelled.',
  answer: 'An answer starts the work that resumes from it.',
};

export interface Undo {
  item: string;
  /** The tier to put back. */
  tier: number;
  /** The tier this write set. The undo is offered only while the item still has it. */
  set: number;
}

/** The undo for a tier change, or why there is none. */
export function undoFor(before: ItemRow, tier: number): Undo | string {
  if (before.tier == null) return 'it had no tier before, and no record removes a tier.';
  if (before.tier === tier) return 'its tier did not change.';
  return { item: before.id, tier: before.tier, set: tier };
}

/** The request that undoes `u`, or why it cannot: someone else changed the tier since, or the item closed. */
export function undoRequest(u: Undo, items: readonly ItemRow[]): Request | string {
  const now = items.find((i) => i.id === u.item);
  if (!now) return `${u.item} is no longer in the log.`;
  if (!OPEN(now)) return `${u.item} is closed now.`;
  if (now.tier === u.tier) return `${u.item}'s tier change has not come back on the log yet; try again in a moment.`;
  if (now.tier !== u.set) return `${u.item}'s tier changed again since; undo would overwrite that.`;
  return request(u.item, { kind: 'tier', tier: u.tier });
}

export type WriteStatus = 'pending' | 'ok' | 'refused' | 'failed';

export interface Write {
  id: number;
  batch: number;
  kind: WriteKind;
  item: string;
  status: WriteStatus;
  /** The tracker's reason for a refusal or failure. */
  reason?: string;
  why?: string;
  undo?: Undo;
  /** Why a tier change written here has no Undo. */
  noUndo?: string;
}

export interface Writes {
  list: Write[];
  /** The batch shown in the activity line, and undone by Undo. */
  last: number | null;
}

export const NO_WRITES: Writes = { list: [], last: null };
const KEEP_BATCHES = 10;

/**
 * Adds or replaces a write by id. Old batches fall off whole so the list stays small,
 * but a pending write never does: Undo, the summary and the double-send guard need every
 * write of a batch however many items it covers.
 */
export function putWrite(w: Writes, write: Write): Writes {
  const at = w.list.findIndex((x) => x.id === write.id);
  const list = at < 0 ? [...w.list, write] : w.list.map((x, i) => (i === at ? write : x));
  const keep = new Set([...new Set(list.map((x) => x.batch))].sort((a, b) => b - a).slice(0, KEEP_BATCHES));
  // Batches can finish out of order; the activity line stays on the newest.
  return { list: list.filter((x) => x.status === 'pending' || keep.has(x.batch)), last: Math.max(w.last ?? 0, write.batch) };
}

/** Why `item` would no longer take the write, or null. The view may be older than the log by a confirm dialog's lifetime. */
function stale(kind: Exclude<WriteKind, 'undo'>, id: string, items: readonly ItemRow[]): string | null {
  const now = items.find((i) => i.id === id);
  if (!now) return `${id} is no longer in the log.`;
  if (CAN[kind](now)) return null;
  return `${id} changed since: it is ${now.stage.replaceAll('_', ' ')} now, so nothing was sent.`;
}

export interface Job {
  item: string;
  req: Request | string;
  undo?: Undo;
  noUndo?: string;
}

/**
 * The writes to send for `ids`, checked against the items as they are now. An item with
 * a write in flight is left out, since a second send would write the same record twice;
 * an item that no longer takes the write is refused here, with the reason, unsent.
 */
export function jobsFor(ids: readonly string[], args: WriteArgs, items: readonly ItemRow[], writes: Writes): Job[] {
  return ids.filter((item) => !pending(writes, item)).map((item) => {
    const why = stale(args.kind, item, items);
    if (why) return { item, req: why };
    const before = items.find((i) => i.id === item)!;
    const undo = args.kind === 'tier' ? undoFor(before, args.tier) : undefined;
    return { item, req: request(item, args), undo: typeof undo === 'object' ? undo : undefined, noUndo: typeof undo === 'string' ? undo : undefined };
  });
}

/** The newest write for an item, which the panel shows next to its actions. */
export function latestFor(w: Writes, item: string): Write | undefined {
  for (let i = w.list.length - 1; i >= 0; i--) if (w.list[i].item === item) return w.list[i];
  return undefined;
}

export function pending(w: Writes, item: string): boolean {
  return w.list.some((x) => x.item === item && x.status === 'pending');
}

const DONE: Readonly<Record<WriteKind, [string, string]>> = {
  accept: ['Accepting', 'Accepted'],
  reject: ['Rejecting', 'Rejected'],
  tier: ['Setting the tier of', 'Set the tier of'],
  close: ['Closing', 'Closed'],
  answer: ['Answering', 'Answered'],
  undo: ['Undoing the tier change on', 'Undid the tier change on'],
};

const items = (ids: string[]) => (ids.length === 1 ? ids[0] : `${ids.length} items`);

/** Whether Undo applies now: every undoable write's tier is on the log and nobody changed it since. */
export function canUndo(batch: readonly Write[], items: readonly ItemRow[]): boolean {
  const undos = batch.filter((x) => x.status === 'ok' && x.undo);
  return undos.length > 0 && undos.every((x) => typeof undoRequest(x.undo!, items) !== 'string');
}

/** One line for the activity area: what is pending, what was written, and each refusal with its reason. */
export function summary(batch: readonly Write[]): { text: string; tone: 'busy' | 'ok' | 'error'; undoable: number } {
  const kind = batch[0]?.kind ?? 'tier';
  const by = (s: WriteStatus) => batch.filter((x) => x.status === s).map((x) => x.item);
  const [doing, done] = DONE[kind];
  const wait = by('pending');
  if (wait.length) return { text: `${doing} ${items(batch.map((x) => x.item))}…`, tone: 'busy', undoable: 0 };
  const ok = by('ok');
  const bad = batch.filter((x) => x.status !== 'ok');
  const parts = [ok.length ? `${done} ${items(ok)}.` : ''];
  // Said here because the Undo button only appears for the writes it can take back.
  const noUndo = new Map<string, string[]>();
  for (const x of batch) if (x.status === 'ok' && x.noUndo) noUndo.set(x.noUndo, [...(noUndo.get(x.noUndo) ?? []), x.item]);
  for (const [why, ids] of noUndo) parts.push(`No undo for ${items(ids)}: ${why}`);
  for (const x of bad) parts.push(`${x.item}: ${x.status === 'refused' ? 'refused' : 'failed'}, ${x.reason}${x.why && x.status === 'refused' ? ` (${x.why})` : ''}`);
  return { text: parts.filter(Boolean).join(' '), tone: bad.length ? 'error' : 'ok', undoable: batch.filter((x) => x.status === 'ok' && x.undo).length };
}
