// The page's view state and a small store. Views read a snapshot and send actions;
// only main.ts talks to the gateway. The item and search are kept in the address so
// a link opens the same view for someone else.
import type { ItemRow } from './fold.ts';

export type Connection =
  | { kind: 'setup' }
  | { kind: 'loading'; what: 'history' | 'reconnecting' }
  | { kind: 'live'; refused: number }
  | { kind: 'retrying'; reason: string; seconds: number }
  | { kind: 'stopped'; reason: string };

export interface ViewState {
  connection: Connection;
  items: ItemRow[];
  query: string;
  selected: string | null;
  /** The item shown in the side panel. */
  open: string | null;
  theme: 'light' | 'dark';
}

export type Action =
  | { type: 'connection'; connection: Connection }
  | { type: 'items'; items: ItemRow[] }
  | { type: 'query'; query: string }
  | { type: 'select'; id: string | null }
  | { type: 'open'; id: string | null }
  | { type: 'theme'; theme: 'light' | 'dark' };

export function reduce(s: ViewState, a: Action): ViewState {
  switch (a.type) {
    case 'connection':
      return { ...s, connection: a.connection };
    case 'items':
      // `open` stays even when the log has no such item: the panel says so rather than the link silently doing nothing.
      return { ...s, items: a.items };
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
      const next = reduce(state, a);
      if (next === state) return;
      state = next;
      for (const l of listeners) l(state);
    },
    subscribe: (l) => void listeners.push(l),
  };
}

/** Reads `q` and `item` from a query string. */
export function fromAddress(search: string): Pick<ViewState, 'query' | 'open'> {
  const q = new URLSearchParams(search);
  return { query: q.get('q') ?? '', open: q.get('item') };
}

/** Writes the view into a query string, keeping every other parameter as it was. */
export function toAddress(search: string, s: Pick<ViewState, 'query' | 'open'>): string {
  const q = new URLSearchParams(search);
  for (const [k, v] of [['q', s.query], ['item', s.open]] as const) {
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
