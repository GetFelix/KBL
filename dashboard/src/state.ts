// The page's view state and a small store. Views read a snapshot and send actions;
// only main.ts talks to the gateway. The view, item and search are kept in the address so
// a link opens the same view for someone else.
import type { BlockedRow, ItemRow } from './fold.ts';

export type Connection =
  | { kind: 'setup' }
  | { kind: 'loading'; what: 'history' | 'reconnecting'; /** The socket is joined and the log is being read. */ linked?: boolean }
  | { kind: 'live'; refused: number }
  | { kind: 'retrying'; reason: string; seconds: number }
  | { kind: 'stopped'; reason: string; advice: Advice };

/** The next step a stop reason backs: a new sign-in link, a reload for a newer page, or nothing to suggest. */
export type Advice = 'sign-in' | 'reload' | null;

/** The advice for a stop code. Only codes that say why get a hint; the rest show the reason alone. */
export function adviceFor(code: string): Advice {
  if (code === 'signed_out' || code === 'forbidden') return 'sign-in';
  if (code === 'newer_format') return 'reload';
  return null;
}

/** Which items the list shows: all of them, or only the ones a link holds back. */
export type View = 'all' | 'blocked';

export interface ViewState {
  connection: Connection;
  /** True once the page has read the whole log and gone live, so an empty list means the log is empty. */
  synced: boolean;
  items: ItemRow[];
  /** The Blocked items with what each waits on, from the same fold as `items`. */
  blocked: BlockedRow[];
  view: View;
  query: string;
  selected: string | null;
  /** The item shown in the side panel. */
  open: string | null;
  theme: 'light' | 'dark';
}

export type Action =
  | { type: 'connection'; connection: Connection }
  | { type: 'items'; items: ItemRow[]; blocked: BlockedRow[] }
  | { type: 'view'; view: View }
  | { type: 'query'; query: string }
  | { type: 'select'; id: string | null }
  | { type: 'open'; id: string | null }
  | { type: 'theme'; theme: 'light' | 'dark' };

export function reduce(s: ViewState, a: Action): ViewState {
  switch (a.type) {
    case 'connection':
      return { ...s, connection: a.connection, synced: s.synced || a.connection.kind === 'live' };
    case 'items':
      // `open` stays even when the log has no such item: the panel says so rather than the link silently doing nothing.
      return { ...s, items: a.items, blocked: a.blocked };
    case 'view':
      return { ...s, view: a.view };
    case 'query':
      return { ...s, query: a.query };
    case 'select':
      return { ...s, selected: a.id };
    case 'open':
      return { ...s, open: a.id, selected: a.id ?? s.selected };
    case 'theme':
      return { ...s, theme: a.theme };
  }
}

export interface Store {
  get(): ViewState;
  dispatch(a: Action): void;
  subscribe(listener: (s: ViewState) => void): void;
}

export function createStore(initial: ViewState): Store {
  let state = initial;
  const listeners: ((s: ViewState) => void)[] = [];
  return {
    get: () => state,
    dispatch(a) {
      state = reduce(state, a);
      for (const l of listeners) l(state);
    },
    subscribe: (l) => void listeners.push(l),
  };
}

export type Address = Pick<ViewState, 'query' | 'open'> & { view?: View };

/** Reads `view`, `q` and `item` from a query string. An unknown view reads as all work. */
export function fromAddress(search: string): Required<Address> {
  const q = new URLSearchParams(search);
  return { view: q.get('view') === 'blocked' ? 'blocked' : 'all', query: q.get('q') ?? '', open: q.get('item') };
}

/** Writes the view into a query string, keeping every other parameter as it was. */
export function toAddress(search: string, s: Address): string {
  const q = new URLSearchParams(search);
  for (const [k, v] of [['view', s.view === 'blocked' ? s.view : null], ['q', s.query], ['item', s.open]] as const) {
    if (v) q.set(k, v);
    else q.delete(k);
  }
  const out = q.toString();
  return out ? `?${out}` : '';
}

/** One line for the status bar, and how loudly to say it. */
export function describe(c: Connection, count: number): { text: string; tone: 'ok' | 'busy' | 'warn' | 'error' } {
  switch (c.kind) {
    case 'setup':
      return { text: 'Not connected', tone: 'warn' };
    case 'loading':
      return { text: c.what === 'history' ? 'Reading history' : 'Reconnecting', tone: 'busy' };
    case 'live': {
      const unread = c.refused ? `, ${c.refused} ${c.refused === 1 ? 'record' : 'records'} this build does not read` : '';
      return { text: `Live, ${count} ${count === 1 ? 'item' : 'items'}${unread}`, tone: c.refused ? 'warn' : 'ok' };
    }
    case 'retrying':
      return { text: `Disconnected: ${c.reason}. Retrying in ${c.seconds} s`, tone: 'warn' };
    case 'stopped':
      return { text: `Stopped: ${c.reason}`, tone: 'error' };
  }
}
