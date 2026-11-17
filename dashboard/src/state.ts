// The page's view state and a small store. Views read a snapshot and send actions;
// only main.ts talks to the gateway. The view, item and search are kept in the address so
// a link opens the same view for someone else.
import type { BlockedRow, ItemRow, ReviewLoad } from './fold.ts';
import { decodeView, encodeView, type Facet, type Filters, type Layout, type SavedView, setFilter, type View } from './views.ts';
import type { Overlaps } from './overlap.ts';
import { putWrite, type Write, type Writes } from './writes.ts';

/** The viewer's sign-in to the tracker, which every write needs. The token itself is held in main.ts only. */
export type TrackerState =
  | { kind: 'signed-out' }
  | { kind: 'checking' }
  | { kind: 'signed-in'; person: string; /** Agent PRs in In review at which agents pause; 0 is no limit. */ reviewLimit: number }
  | { kind: 'refused'; reason: string };

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

export type { View };

export interface ViewState {
  connection: Connection;
  /** True once the page has read the whole log and gone live, so an empty list means the log is empty. */
  synced: boolean;
  items: ItemRow[];
  /** The Blocked items with what each waits on, from the same fold as `items`. */
  blocked: BlockedRow[];
  /** File overlaps per item, from the same fold as `items`. */
  overlaps: Overlaps;
  /** The tracker's review load, from the same fold as `items`. */
  reviewLoad: ReviewLoad;
  view: View;
  query: string;
  selected: string | null;
  /** The item shown in the side panel. */
  open: string | null;
  theme: 'light' | 'dark';
  layout: Layout;
  filters: Filters;
  density: Density;
  /** This viewer's saved views, from local storage. */
  saved: SavedView[];
  /** Items picked for a bulk action, in the order they were picked. */
  marked: string[];
  writes: Writes;
  tracker: TrackerState;
  /** The text of each open question by its offset, read from the record as it was folded. */
  questions: ReadonlyMap<number, string>;
}

export type Density = 'comfortable' | 'compact';

export type Action =
  | { type: 'connection'; connection: Connection }
  | { type: 'items'; items: ItemRow[]; blocked: BlockedRow[]; overlaps: Overlaps; reviewLoad?: ReviewLoad; questions?: ReadonlyMap<number, string> }
  /** Marks or unmarks one item for a bulk action. */
  | { type: 'mark'; id: string }
  | { type: 'marks'; ids: string[] }
  | { type: 'write'; write: Write }
  | { type: 'tracker'; tracker: TrackerState }
  | { type: 'view'; view: View }
  | { type: 'query'; query: string }
  | { type: 'select'; id: string | null }
  | { type: 'open'; id: string | null }
  | { type: 'theme'; theme: 'light' | 'dark' }
  | { type: 'layout'; layout: Layout }
  | { type: 'filter'; facet: Facet; value: string | null }
  | { type: 'clearFilters' }
  | { type: 'density'; density: Density }
  | { type: 'saved'; saved: SavedView[] }
  /** Opens a saved view: its scope, layout, search and filters replace the current ones. */
  | { type: 'openView'; search: string };

export function reduce(s: ViewState, a: Action): ViewState {
  switch (a.type) {
    case 'connection':
      return { ...s, connection: a.connection, synced: s.synced || a.connection.kind === 'live' };
    case 'items':
      // `open` stays even when the log has no such item: the panel says so rather than the link silently doing nothing.
      // A mark on an item the log no longer has would act on nothing.
      return {
        ...s,
        items: a.items,
        blocked: a.blocked,
        overlaps: a.overlaps,
        reviewLoad: a.reviewLoad ?? s.reviewLoad,
        questions: a.questions ?? s.questions,
        marked: s.marked.filter((id) => a.items.some((i) => i.id === id)),
      };
    case 'mark':
      return { ...s, marked: s.marked.includes(a.id) ? s.marked.filter((x) => x !== a.id) : [...s.marked, a.id] };
    case 'marks':
      return { ...s, marked: a.ids };
    case 'write':
      return { ...s, writes: putWrite(s.writes, a.write) };
    case 'tracker':
      return { ...s, tracker: a.tracker };
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
    case 'layout':
      return { ...s, layout: a.layout };
    case 'filter':
      return { ...s, filters: setFilter(s.filters, a.facet, a.value) };
    case 'clearFilters':
      return { ...s, filters: {} };
    case 'density':
      return { ...s, density: a.density };
    case 'saved':
      return { ...s, saved: a.saved };
    case 'openView':
      return { ...s, ...decodeView(a.search) };
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

export type AddressState = Pick<ViewState, 'view' | 'query' | 'open' | 'layout' | 'filters'>;

/** Reads the view (`view`, `layout`, `q`, filters) and `item` from a query string. */
export function fromAddress(search: string): AddressState {
  return { ...decodeView(search), open: new URLSearchParams(search).get('item') };
}

/** Writes the view into a query string, keeping every other parameter as it was. */
export function toAddress(search: string, s: AddressState): string {
  const q = new URLSearchParams(encodeView(search, s));
  if (s.open) q.set('item', s.open);
  else q.delete('item');
  const out = q.toString();
  return out ? `?${out}` : '';
}

/** The header line when the review load is at the tracker's limit: the same count the pause uses, split into its parts. */
export function reviewFull(load: ReviewLoad, limit: number): string | null {
  if (limit <= 0 || load.load < limit) return null;
  const held = load.load - load.inReview;
  const parts = [load.inReview > 0 ? `${load.inReview} in review` : '', held > 0 ? `${held} in progress` : ''].filter(Boolean);
  return `review full: agents paused, ${load.load}/${limit}: ${parts.join(', ')}`;
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
