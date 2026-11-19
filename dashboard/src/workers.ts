// Supervisors and the workers they started, from the fold's `workers_json()` and the item rows.
// Recorded fields only: who started whom, what each worker holds now, and how it ended.
// No DOM here. The Workers dialog draws it and main.ts sends the stop requests.
import type { ItemRow } from './fold.ts';
import type { Outcome, Request } from './writes.ts';

/** One worker as the fold records it. */
export interface WorkerRow {
  worker: string;
  supervisor: string;
  vendor: string | null;
  profile: string | null;
  model: string | null;
  started_ms: number;
  stopped_ms: number | null;
  /** It ended by its own `worker_finished` (format 24), not a stop. */
  finished: boolean;
  holds: { item: string; attempt: number } | null;
}

/** How a worker stands: running with an item, running with nothing, or over. */
export type WorkerState = 'working' | 'idle' | 'finished' | 'stopped';

/** What a worker holds, with the item's recorded title, stage and last write. */
export interface Hold {
  item: string;
  attempt: number;
  title: string | null;
  stage: string | null;
  /** When the holder last wrote on the item, from the item's `active_ms`. */
  active_ms: number | null;
}

export interface WorkerLine extends WorkerRow {
  state: WorkerState;
  hold: Hold | null;
}

/** A supervisor and every worker it started, live ones first. */
export interface Group {
  supervisor: string;
  workers: WorkerLine[];
  /** Workers that can still be stopped: working or idle. */
  live: number;
}

export function workers(fold: { workersJson(): string }): WorkerRow[] {
  return JSON.parse(fold.workersJson()) as WorkerRow[];
}

export function stateOf(w: WorkerRow): WorkerState {
  if (w.stopped_ms === null) return w.holds ? 'working' : 'idle';
  return w.finished ? 'finished' : 'stopped';
}

export const STATE_TEXT: Readonly<Record<WorkerState, string>> = {
  working: 'Working',
  idle: 'Idle',
  finished: 'Finished',
  stopped: 'Stopped',
};

const RANK: Readonly<Record<WorkerState, number>> = { working: 0, idle: 1, finished: 2, stopped: 3 };

/** `anthropic · sonnet`, from the fields the supervisor recorded when it started the worker; empty when none. */
export function launchText(w: Pick<WorkerRow, 'vendor' | 'profile' | 'model'>): string {
  return [w.vendor, w.model ?? w.profile].filter(Boolean).join(' · ');
}

/**
 * Groups workers under the supervisor that started them. Groups with a live worker come first,
 * then by name; inside a group live workers come first, then the most recently started. A worker
 * that is over keeps its row, so the group still says how the pool ended.
 */
export function groups(rows: readonly WorkerRow[], items: readonly ItemRow[]): Group[] {
  const byId = new Map(items.map((i) => [i.id, i]));
  const by = new Map<string, WorkerLine[]>();
  for (const w of rows) {
    const item = w.holds ? byId.get(w.holds.item) : undefined;
    const hold: Hold | null = w.holds
      ? { item: w.holds.item, attempt: w.holds.attempt, title: item?.title ?? null, stage: item?.stage ?? null, active_ms: item?.active_ms ?? null }
      : null;
    const line: WorkerLine = { ...w, state: stateOf(w), hold };
    by.set(w.supervisor, [...(by.get(w.supervisor) ?? []), line]);
  }
  const out = [...by.entries()].map(([supervisor, list]): Group => {
    const sorted = [...list].sort((a, b) => RANK[a.state] - RANK[b.state] || b.started_ms - a.started_ms || a.worker.localeCompare(b.worker));
    return { supervisor, workers: sorted, live: sorted.filter((w) => w.state === 'working' || w.state === 'idle').length };
  });
  return out.sort((a, b) => Number(b.live > 0) - Number(a.live > 0) || a.supervisor.localeCompare(b.supervisor));
}

/** The workers that can be stopped now, across every group. */
export const liveCount = (gs: readonly Group[]): number => gs.reduce((n, g) => n + g.live, 0);

/** `3 working, 1 idle, 2 finished`: only the states that occur. */
export function summary(g: Pick<Group, 'workers'>): string {
  const n = new Map<WorkerState, number>();
  for (const w of g.workers) n.set(w.state, (n.get(w.state) ?? 0) + 1);
  return (Object.keys(STATE_TEXT) as WorkerState[])
    .filter((s) => n.has(s))
    .map((s) => `${n.get(s)} ${STATE_TEXT[s].toLowerCase()}`)
    .join(', ');
}

/** The tracker request that stops one worker. A person may stop any worker; the tracker decides. */
export function stopRequest(worker: string): Request {
  return { path: '/v1/work/workers/stop', body: { worker } };
}

/** Names of the live workers of a group, or of every group when `supervisor` is null. */
export function liveNames(gs: readonly Group[], supervisor: string | null): string[] {
  return gs.filter((g) => supervisor === null || g.supervisor === supervisor).flatMap((g) => g.workers.filter((w) => w.state === 'working' || w.state === 'idle').map((w) => w.worker));
}

/**
 * Stops each worker in turn with `send`. A worker the tracker says is already stopped (`bad_worker`) is where the
 * person wanted it, so it is not a failure. Resolves to why the first other refusal happened, or null when all stopped.
 */
export async function stopEach(send: (r: Request) => Promise<Outcome>, names: readonly string[]): Promise<string | null> {
  for (const name of names) {
    const o = await send(stopRequest(name));
    if (o.status === 'ok' || (o.status === 'refused' && o.why === 'bad_worker')) continue;
    return `${name}: ${o.reason}`;
  }
  return null;
}
