// What a person can do to an item, the tracker request for each, how its answer reads,
// and which writes can be undone. No DOM: main.ts sends the requests and the views show
// the results. Every write is one of the tracker's person routes, sent with the viewer's
// own token, so the page can never do more than the person could with curl.
import { ADVICE_FORMAT, canAdvise } from './advice.ts';
import type { ItemRow } from './fold.ts';
import type { DecisionLine } from './story.ts';

export type WriteKind = 'accept' | 'reject' | 'tier' | 'close' | 'answer' | 'flag' | 'unflag' | 'rate' | 'take' | 'reassign' | 'steer' | 'advice' | 'redo' | 'undo';

export interface Request {
  path: string;
  body: Record<string, unknown>;
  /** Sent first; its offset goes into `body.note`. A redo's note is its own record, which the redo names. */
  note?: Request;
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
  merge_disputed: 'Marked merged, but upstream shows it open',
  taken: 'Taken by a person',
};

export function needsText(needs: string | null | undefined): string {
  return needs ? (NEEDS[needs] ?? needs.replaceAll('_', ' ')) : '';
}

/** The person an item is assigned to, or null. An agent assignee has no person name. */
export const assigneePerson = (i: Pick<ItemRow, 'assignee'>): string | null => (i.assignee && 'person' in i.assignee ? i.assignee.person : null);

/** The agent an item is assigned to, or null. */
export const assigneeAgent = (i: Pick<ItemRow, 'assignee'>): string | null => (i.assignee && 'agent' in i.assignee ? i.assignee.agent : null);

/** Why the item waits on a person, naming who took it when that is the reason. */
export function needsLine(i: Pick<ItemRow, 'needs' | 'assignee'>): string {
  const who = assigneePerson(i);
  return i.needs === 'taken' && who ? `Taken by ${who}` : needsText(i.needs);
}

/** Up to two initials for an assignee chip: `ana lopez` is AL, `ana` is AN. */
export function initials(name: string): string {
  const parts = name.split(/[\s._-]+/).filter(Boolean);
  const letters = parts.length > 1 ? parts[0][0] + parts[1][0] : (parts[0] ?? '?').slice(0, 2);
  return letters.toUpperCase();
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

/** The first log format that takes `flagged` records. */
export const FLAG_FORMAT = 7;
const FLAGGABLE_LOG = (i: ItemRow) => (i.format ?? 0) >= FLAG_FORMAT;

/** Whether the log is at a format that takes flags. The palette lists the flag commands only then. */
export const logTakesFlags = (items: readonly ItemRow[]) => items.some(FLAGGABLE_LOG);
/** The first log format that takes `summary_rated` records. */
export const RATE_FORMAT = 11;

/**
 * Whether the page may offer a mark on the summary of an attempt. Not part of `CAN`: a closed
 * item's summary can still be marked, which would hide the closed notice there. A log below
 * format 11 refuses every `summary_rated` record, so no button shows there.
 */
export const canRate = (i: ItemRow): boolean => (i.format ?? 0) >= RATE_FORMAT;

/** The first log format that takes `taken`, `reassigned` and `steered` records. */
export const ASSIGN_FORMAT = 12;
const ASSIGNABLE_LOG = (i: ItemRow) => (i.format ?? 0) >= ASSIGN_FORMAT;

/** Whether the log is at a format that takes assignees. The palette lists the assign command only then. */
export const logTakesAssignees = (items: readonly ItemRow[]) => items.some(ASSIGNABLE_LOG);

/** Whether the page may offer an assignee on this item. The tracker refuses an item nobody accepted. */
export const canAssign = (i: ItemRow): boolean => ASSIGNABLE_LOG(i) && OPEN(i) && i.accepted === true;

/** The attempt an agent holds, from the fold's `held_attempt`, else the newest attempt of its story still open. */
export function heldAttempt(i: ItemRow, story?: { attempts: readonly { attempt: number; done: boolean; closed: boolean }[] }): number | null {
  if (i.stage !== 'held') return null;
  if (i.held_attempt != null) return i.held_attempt;
  return story?.attempts.filter((a) => !a.done && !a.closed).at(-1)?.attempt ?? null;
}

/** Whether a note can go to the agent holding the item. */
export const canSteer = (i: ItemRow): boolean => ASSIGNABLE_LOG(i) && i.stage === 'held';

/** Whether the item stopped because its attempt used its tries, where `viewer` can take it: not when it is theirs already. */
export const canTakeAfterLimit = (i: ItemRow, viewer: string | null): boolean =>
  canAssign(i) && i.needs === 'tries_used' && (viewer === null || assigneePerson(i) !== viewer);

/** What a quick choice in the assign picker writes, or null when the item is already so. The fold would refuse it as `no_change`. */
export function assignPick(now: string | null, me: string, choice: 'me' | 'nobody'): WriteArgs | null {
  if (choice === 'me') return now === me ? null : { kind: 'take' };
  return now === null ? null : { kind: 'reassign', to: null };
}

/** What `a` does on `id`: nothing for an item that takes no assignee, else sign in first, else the item. */
export function assignTarget(items: readonly ItemRow[], id: string, signedIn: boolean): ItemRow | 'sign-in' | null {
  const item = items.find((i) => i.id === id);
  if (!item || !canAssign(item)) return null;
  return signedIn ? item : 'sign-in';
}

export const FLAG_COMMANDS: ReadonlySet<string> = new Set(['flag', 'unflag']);

/**
 * The marks left once an action went to `ids`. Acting on marked items spends their marks, as in GitHub;
 * a mark on an item the action did not reach stays. An action on items outside the selection leaves it alone.
 */
export function marksAfter(marked: readonly string[], ids: readonly string[]): string[] {
  return ids.length > 0 && ids.every((id) => marked.includes(id)) ? marked.filter((id) => !ids.includes(id)) : [...marked];
}

const OPEN = (i: ItemRow) => i.stage !== 'done' && i.stage !== 'closed';

/** Which writes the fold would take for this item. The tracker still decides; this only hides what it would refuse. */
export const CAN: Readonly<Record<Exclude<WriteKind, 'undo' | 'rate' | 'take' | 'reassign' | 'steer' | 'advice' | 'redo'>, (i: ItemRow) => boolean>> = {
  accept: (i) => i.stage === 'triage' || i.stage === 'proposed',
  // The fold rejects only a proposal nobody accepted; an imported root is closed instead.
  reject: (i) => i.stage === 'proposed',
  tier: OPEN,
  close: OPEN,
  answer: (i) => (i.needs === 'question' || i.needs === 'unclear') && i.question != null,
  // The fold refuses a flag on an item nobody accepted (it waits in Triage already) and a repeat of either.
  // A log below format 7 refuses every `flagged` record, so neither button shows there.
  flag: (i) => FLAGGABLE_LOG(i) && OPEN(i) && i.accepted === true && i.flagged !== true,
  unflag: (i) => FLAGGABLE_LOG(i) && OPEN(i) && i.flagged === true,
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
  | { kind: 'answer'; question: number; text: string; chosen: string | null }
  | { kind: 'flag' }
  | { kind: 'unflag' }
  /** Marks the summary an attempt handed off as accurate or not. The latest mark stands. */
  | { kind: 'rate'; attempt: number; accurate: boolean }
  /** Assigns the item to the signed-in person. */
  | { kind: 'take' }
  /** Assigns the item to the named person, or the named agent with `agent`, or to nobody with null. */
  | { kind: 'reassign'; to: string | null; agent?: true }
  /** Sends a note to the agent holding `attempt`. */
  | { kind: 'steer'; attempt: number; text: string }
  /** Starts a new attempt from the decision at offset `at` on `fromAttempt`. `note` may be empty. */
  | { kind: 'redo'; fromAttempt: number; at: number; note: string }
  /** Replaces triage's dependencies and split on the answer at offset `proposal`. */
  | { kind: 'advice'; depends_on: string[]; split: string[]; proposal: number };

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
    case 'flag':
      return { path: '/v1/work/flag', body: { item } };
    case 'unflag':
      return { path: '/v1/work/unflag', body: { item } };
    case 'rate':
      return { path: '/v1/work/rate-summary', body: { item, attempt: a.attempt, accurate: a.accurate } };
    case 'take':
      return { path: '/v1/work/take', body: { item } };
    case 'reassign':
      return { path: '/v1/work/reassign', body: { item, to: a.to === null ? null : a.agent ? { agent: a.to } : { person: a.to } } };
    case 'steer':
      return { path: '/v1/work/steer', body: { item, attempt: a.attempt, text: { text: a.text.trim() } } };
    case 'redo': {
      const body = { item, from_attempt: a.fromAttempt, at: a.at };
      const text = a.note.trim();
      return text ? { path: '/v1/work/redo', body, note: { path: '/v1/work/note', body: { item, text: { text } } } } : { path: '/v1/work/redo', body };
    }
    case 'advice':
      return { path: '/v1/work/proposals/edit', body: { item, depends_on: a.depends_on, split: a.split, proposal: a.proposal } };
    case 'answer': {
      const body: Record<string, unknown> = { item, question: a.question };
      // The tracker reads `text` as content: inline text is `{ text }`, never a bare string.
      if (a.text.trim()) body.text = { text: a.text.trim() };
      if (a.chosen !== null) body.chosen = a.chosen;
      return { path: '/v1/work/questions/answer', body };
    }
  }
}

/** Open attempts one item may have at once, as the fold counts them. */
export const OPEN_ATTEMPTS = 2;

/** The story fields a redo check reads. */
type RedoStory = { attempts: readonly { attempt: number; closed: boolean }[] };

/**
 * Why `item` cannot be redone from a decision on `attempt`, or null. The fold decides; this
 * hides what it would refuse so the button is not offered for it. A closed item, one an agent
 * holds, one with a redo already waiting, one out of redos, and one with its open attempts used.
 */
export function redoBlock(item: ItemRow, story: RedoStory | undefined, attempt: number): string | null {
  if (!OPEN(item)) return 'The item is closed.';
  if (item.stage === 'held' || item.held_attempt != null) return 'An agent holds the item now. A redo starts once its attempt ends.';
  if (item.redo_pending) return 'A redo already waits for an agent.';
  if (item.redos_left != null && item.redos_left <= 0) return 'No redos are left for this item.';
  if (!story?.attempts.some((a) => a.attempt === attempt)) return 'That attempt is not in the story.';
  if (story.attempts.filter((a) => !a.closed).length >= OPEN_ATTEMPTS) return 'Two attempts are open. Close one before redoing.';
  return null;
}

/** Whether a red check on the item's open pull request means a redo must carry a note. */
export const redoNeedsNote = (i: Pick<ItemRow, 'pr_phase' | 'pr_checks'>): boolean => i.pr_phase === 'open' && i.pr_checks === 'red';

/** What the redo dialog would send, or what is wrong with it. */
export function checkRedo(note: string, needsNote: boolean): string | null {
  const text = note.trim();
  if (needsNote && !text) return 'A check failed on the pull request. Write a note saying what to do differently.';
  if (new TextEncoder().encode(text).length > 4096) return 'Keep the note under 4 KiB, or link to the longer text.';
  return null;
}

/** What the redo dialog says it will do, so the words are tested without a page. */
export function redoCopy(item: Pick<ItemRow, 'id' | 'redos_left'>, d: DecisionLine): { title: string; does: string; budget: string } {
  const left = item.redos_left;
  return {
    title: `Redo ${item.id} from decision ${d.offset}?`,
    does: `A new attempt starts on ${item.id}, branching from decision ${d.offset} on attempt ${d.attempt} (${d.chosen}). It starts from the commit recorded with that decision; the attempt it branches from stays in the story.`,
    budget: left == null
      ? 'This counts against the redos the item may take.'
      : `This counts against the redos the item may take. ${left} left now, ${Math.max(left - 1, 0)} after this one.`,
  };
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

/** What a steer note would send, or what is wrong with it. */
export function checkSteer(text: string): string | null {
  if (!text.trim()) return 'Write the note for the agent.';
  if (new TextEncoder().encode(text.trim()).length > 4096) return 'Keep the note under 4 KiB, or link to the longer text.';
  return null;
}

/** The name an assign picker sends, or why it is empty. */
export function checkPerson(name: string): string | null {
  return name.trim() ? null : 'Write the name of the person.';
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
  // Each takes the other back, so neither asks first.
  flag: null,
  unflag: null,
  // Marking the other way replaces the mark, so there is nothing to ask first.
  rate: null,
  // Another assign sets each back, so none asks first.
  take: null,
  reassign: null,
  steer: 'A note goes to the agent as it works; it cannot be taken back.',
  redo: 'The new attempt starts at once. It counts against the redos this item may take, and no record takes it back.',
  // Editing again sets it back, so it does not ask first.
  advice: null,
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
  if (kind === 'take' || kind === 'reassign') return canAssign(now) ? null : `${id} cannot be assigned now, so nothing was sent.`;
  if (kind === 'steer') return canSteer(now) ? null : `${id} is not held by an agent now, so nothing was sent.`;
  if (kind === 'advice') return canAdvise(now) ? null : `${id} is not a proposal that can be adjusted now, so nothing was sent.`;
  if (kind === 'redo') return null;
  if (kind === 'rate') return canRate(now) ? null : `The log is below format ${RATE_FORMAT}, so nothing was sent.`;
  if (CAN[kind](now)) return null;
  if (kind === 'flag' && OPEN(now) && now.flagged) return `${id} is already flagged, so nothing was sent.`;
  if (kind === 'unflag' && OPEN(now)) return `${id} is not flagged now, so nothing was sent.`;
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
    // Triage answering again replaces what the person read; the tracker refuses it, and so do we before sending.
    if (args.kind === 'advice' && before.proposal?.at !== args.proposal) return { item, req: `Triage answered ${item} again since you opened it, so nothing was sent. Read its new answer and adjust that.` };
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
  flag: ['Flagging', 'Flagged'],
  rate: ['Marking the summary of', 'Marked the summary of'],
  take: ['Taking', 'Took'],
  reassign: ['Assigning', 'Assigned'],
  steer: ['Sending the note on', 'Sent the note on'],
  redo: ['Starting a redo of', 'Started a redo of'],
  advice: ['Adjusting the advice on', 'Adjusted the advice on'],
  unflag: ['Unflagging', 'Unflagged'],
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
  for (const x of bad) {
    if (x.status === 'refused' && x.why === 'format') parts.push(`${x.item}: refused, raise the log to format ${kind === 'rate' ? `${RATE_FORMAT} to mark summaries` : kind === 'take' || kind === 'reassign' || kind === 'steer' ? `${ASSIGN_FORMAT} to assign and steer` : kind === 'advice' ? `${ADVICE_FORMAT} to adjust triage’s advice` : `${FLAG_FORMAT} to use Needs-you flags`}.`);
    else parts.push(`${x.item}: ${x.status === 'refused' ? 'refused' : 'failed'}, ${x.reason}${x.why && x.status === 'refused' ? ` (${x.why})` : ''}`);
  }
  return { text: parts.filter(Boolean).join(' '), tone: bad.length ? 'error' : 'ok', undoable: batch.filter((x) => x.status === 'ok' && x.undo).length };
}
