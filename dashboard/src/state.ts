// The page's view state and a small store. Views read a snapshot and send actions;
// only main.ts talks to the gateway. The view, item and search are kept in the address so
// a link opens the same view for someone else.
import type { BlockedRow, ItemRow, ReviewLoad } from './fold.ts';
import { defaultGroup } from './group-by.ts';
import { selectionFor, settleSelection, visible } from './list.ts';
import { decodeView, encodeView, type Facet, type Filters, type Layout, type SavedView, setFilter, type View } from './views.ts';
import type { Overlaps } from './overlap.ts';
import type { Activities } from './activity.ts';
import type { Handoffs } from './handoff.ts';
import type { InboxKind, Waiting } from './inbox.ts';
import type { Stories } from './story.ts';
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
  /** The story of each item with a recorded decision, from the same fold as `items`. */
  stories: Stories;
  /** What each item's latest finished attempt handed off, from the same fold as `items`. */
  handoffs?: Handoffs;
  /** The tracker's review load, from the same fold as `items`. */
  reviewLoad: ReviewLoad;
  /** What happened on the open item and its children, from the same fold as `items`. */
  activity?: Activities;
  view: View;
  query: string;
  selected: string | null;
  /** The item shown in the side panel. */
  open: string | null;
  theme: 'light' | 'dark';
  layout: Layout;
  filters: Filters;
  /** What the list groups by; absent is the view's default. Part of the address and of a saved view. */
  group?: string;
  /** Folded-away groups, as `collapseKey`s. Per view and grouping; not part of the address. */
  collapsed?: string[];
  density: Density;
  /** Child work (triage, augment, review) shows as cards of its own instead of under its root. */
  childCards?: boolean;
  /** Roots whose children are listed open under the card. */
  expanded?: string[];
  /** This viewer's saved views, from local storage. */
  saved: SavedView[];
  /** Items picked for a bulk action, in the order they were picked. */
  marked: string[];
  writes: Writes;
  tracker: TrackerState;
  /** The text of each open question by its offset, read from the record as it was folded. */
  questions: ReadonlyMap<number, string>;
  /** The kind of inbox item shown in Needs you, or null for every kind. Not part of the address. */
  kind: InboxKind | null;
  /** When each item began waiting on a person, as a log offset. */
  waiting: ReadonlyMap<string, Waiting>;
}

export type Density = 'comfortable' | 'compact';

export type Action =
  | { type: 'connection'; connection: Connection }
  | { type: 'items'; items: ItemRow[]; blocked: BlockedRow[]; overlaps: Overlaps; stories?: Stories; handoffs?: Handoffs; reviewLoad?: ReviewLoad; questions?: ReadonlyMap<number, string>; waiting?: ReadonlyMap<string, Waiting>; activity?: Activities }
  | { type: 'kind'; kind: InboxKind | null }
  /** What happened on the open item, read when it opens or the log moves. */
  | { type: 'activity'; activity: Activities }
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
  /** Picks what the list groups by; undefined goes back to the view's default. */
  | { type: 'group'; group: string | undefined }
  /** Folds one group away, or opens it; `collapsed` forces the state. */
  | { type: 'collapse'; key: string; collapsed?: boolean }
  /** Folds or opens every group the list shows. */
  | { type: 'collapseAll'; collapsed: boolean }
  | { type: 'density'; density: Density }
  | { type: 'childCards'; on: boolean }
  /** Opens or closes the child list under one root. */
  | { type: 'expand'; id: string }
  | { type: 'saved'; saved: SavedView[] }
  /** Opens a saved view: its scope, layout, search and filters replace the current ones. */
  | { type: 'openView'; search: string };

/** The state with its selection moved off any row the groups hide. */
const settled = (s: ViewState): ViewState => ({ ...s, selected: settleSelection(s) });

export function reduce(s: ViewState, a: Action): ViewState {
  switch (a.type) {
    case 'connection':
      return { ...s, connection: a.connection, synced: s.synced || a.connection.kind === 'live' };
    case 'items':
      // `open` stays even when the log has no such item: the panel says so rather than the link silently doing nothing.
      // A mark on an item the log no longer has would act on nothing.
      const next: ViewState = {
        ...s,
        items: a.items,
        blocked: a.blocked,
        overlaps: a.overlaps,
        stories: a.stories ?? s.stories,
        handoffs: a.handoffs ?? s.handoffs,
        reviewLoad: a.reviewLoad ?? s.reviewLoad,
        questions: a.questions ?? s.questions,
        activity: a.activity ?? s.activity,
        waiting: a.waiting ?? s.waiting,
        marked: s.marked.filter((id) => a.items.some((i) => i.id === id)),
      };
      // A link straight to a nested child seeds the selection before the items arrive.
      const seeded = next.selected && next.selected === next.open ? { ...next, selected: selectionFor(next, next.selected) } : next;
      // An update can land an item in a collapsed group; the selection must not sit on a row that is not drawn.
      return { ...seeded, selected: settleSelection(seeded) };
    case 'activity':
      return { ...s, activity: a.activity };
    case 'mark':
      return { ...s, marked: s.marked.includes(a.id) ? s.marked.filter((x) => x !== a.id) : [...s.marked, a.id] };
    case 'marks':
      return { ...s, marked: a.ids };
    case 'write':
      return { ...s, writes: putWrite(s.writes, a.write) };
    case 'tracker':
      return { ...s, tracker: a.tracker };
    case 'view':
      return settled({ ...s, view: a.view });
    case 'kind':
      return { ...s, kind: a.kind };
    case 'query':
      return { ...s, query: a.query };
    case 'select':
      return { ...s, selected: a.id };
    case 'open':
      // The activity read is for the item that was open; the next read follows right behind.
      return { ...s, open: a.id, selected: a.id ? selectionFor(s, a.id) : s.selected, activity: a.id === s.open ? s.activity : undefined };
    case 'theme':
      return { ...s, theme: a.theme };
    case 'layout':
      return settled({ ...s, layout: a.layout });
    case 'filter':
      return { ...s, filters: setFilter(s.filters, a.facet, a.value) };
    case 'clearFilters':
      return { ...s, filters: {} };
    case 'group': {
      // Picking the view's own default is no choice, so it follows the view when that changes.
      const next = { ...s, group: a.group === defaultGroup(s.view) ? undefined : a.group };
      return { ...next, selected: settleSelection(next) };
    }
    case 'collapse': {
      const was = (s.collapsed ?? []).includes(a.key);
      const fold = a.collapsed ?? !was;
      const next = { ...s, collapsed: fold ? (was ? s.collapsed : [...(s.collapsed ?? []), a.key]) : (s.collapsed ?? []).filter((k) => k !== a.key) };
      return { ...next, selected: settleSelection(next) };
    }
    case 'collapseAll': {
      const keys = visible(s).flatMap((g) => (g.toggle ? [g.toggle] : []));
      const rest = (s.collapsed ?? []).filter((k) => !keys.includes(k));
      const next = { ...s, collapsed: a.collapsed ? [...rest, ...keys] : rest };
      return { ...next, selected: settleSelection(next) };
    }
    case 'density':
      return { ...s, density: a.density };
    case 'childCards':
      return { ...s, childCards: a.on };
    case 'expand': {
      const open = s.expanded ?? [];
      return { ...s, expanded: open.includes(a.id) ? open.filter((x) => x !== a.id) : [...open, a.id] };
    }
    case 'saved':
      return { ...s, saved: a.saved };
    case 'openView':
      return settled({ ...s, group: undefined, ...decodeView(a.search), kind: null });
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

export type AddressState = Pick<ViewState, 'view' | 'query' | 'open' | 'layout' | 'filters' | 'group'>;

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
