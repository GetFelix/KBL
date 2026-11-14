// Connect to felix-gateway, fold work.events with the core crate as WebAssembly,
// and show the items. Settings come from the query string:
// ?gateway=ws://127.0.0.1:8787/ws&room=kbl&token=<ID token>&stream=events
// This file owns the side effects: the gateway loop, the address and the keys.
// The views under ui/ only render the store.
import './ui/tokens.css';
import './ui/app.css';
import init, { Folder } from './wasm/kbl_wasm.js';
import { items, type Fold } from './fold.ts';
import { follow, FollowError } from './follow.ts';
import { connect, GatewayError } from './gateway.ts';
import { commandFor, type CommandId } from './keys.ts';
import { flatten, move, moveBoard, visible } from './list.ts';
import { adviceFor, createStore, fromAddress, toAddress, type Density, type ViewState } from './state.ts';
import { deleteView, parseSaved, renameView, saveView, type SavedView } from './views.ts';
import { helpView } from './ui/help-view.ts';
import { paletteView } from './ui/palette-view.ts';
import { shell } from './ui/shell.ts';
import { viewDialog } from './ui/view-dialog.ts';

const mac = /Mac|iPhone|iPad/.test(navigator.platform);
const q = new URLSearchParams(location.search);
const token = q.get('token');
// Keep the token in memory only: out of the address bar, history and any shared link.
if (token) {
  q.delete('token');
  history.replaceState(null, '', `${location.pathname}${q.size ? `?${q}` : ''}${location.hash}`);
}
// Saved views and density are per viewer, so they live in this browser. Storage can
// be missing or throw (private windows, blocked site data); the page works without it.
const VIEWS_KEY = 'kbl.dashboard.views';
const DENSITY_KEY = 'kbl.dashboard.density';
const stored = (key: string): string | null => {
  try {
    return localStorage.getItem(key);
  } catch {
    return null;
  }
};
const store_ = (key: string, value: string) => {
  try {
    localStorage.setItem(key, value);
  } catch {
    // Not remembered past this page; nothing else depends on it.
  }
};
const themeParam = q.get('theme');
const address = fromAddress(location.search);
const initial: ViewState = {
  connection: q.get('room') && token ? { kind: 'loading', what: 'history' } : { kind: 'setup' },
  synced: false,
  items: [],
  ...address,
  selected: address.open,
  density: stored(DENSITY_KEY) === 'compact' ? 'compact' : 'comfortable',
  saved: parseSaved(stored(VIEWS_KEY)),
  theme: themeParam === 'light' || themeParam === 'dark' ? themeParam : matchMedia('(prefers-color-scheme: light)').matches ? 'light' : 'dark',
};
const store = createStore(initial);

const openItem = (id: string) => {
  store.dispatch({ type: 'open', id });
  ui.panel.focus();
};
const closeItem = () => {
  const before = store.get();
  store.dispatch({ type: 'open', id: null });
  // The open item may have no visible row (filtered out, or not in the log): keep the focus somewhere real.
  const rows = flatten(visible(before.items, before.query, before.filters));
  if (before.open && rows.some((r) => r.id === before.open)) return ui.list.focusSelected();
  const first = rows[0]?.id;
  if (first) {
    store.dispatch({ type: 'select', id: first });
    ui.list.focusSelected();
  } else ui.search.focus();
};

const run = (id: CommandId): void => {
  const s = store.get();
  switch (id) {
    case 'palette':
      return palette.open();
    case 'help':
      return help.open();
    case 'search':
      ui.search.focus();
      return ui.search.select();
    case 'next':
    case 'previous':
    case 'left':
    case 'right':
    case 'first':
    case 'last': {
      const groups = visible(s.items, s.query, s.filters);
      const rows = flatten(groups);
      const board = s.layout === 'board';
      if (!board && (id === 'left' || id === 'right')) return;
      const dir = ({ next: 'down', previous: 'up', left: 'left', right: 'right' } as const)[id as 'next'];
      const target =
        id === 'first'
          ? (rows[0]?.id ?? null)
          : id === 'last'
            ? (rows.at(-1)?.id ?? null)
            : board
              ? moveBoard(groups, s.selected, dir)
              : move(rows, s.selected, id === 'next' ? 1 : -1);
      store.dispatch({ type: 'select', id: target });
      return ui.list.focusSelected();
    }
    case 'open':
      if (s.selected && flatten(visible(s.items, s.query, s.filters)).some((r) => r.id === s.selected)) openItem(s.selected);
      return;
    case 'close':
      if (ui.menuOpen()) return ui.closeMenu();
      if (document.activeElement === ui.search) ui.search.blur();
      if (s.open) closeItem();
      else if (s.query) store.dispatch({ type: 'query', query: '' });
      return;
    case 'theme':
      return store.dispatch({ type: 'theme', theme: s.theme === 'dark' ? 'light' : 'dark' });
    case 'layout': {
      // Keep the keyboard where it was: on the selected item in the new layout.
      const onItem = ui.list.el.contains(document.activeElement);
      store.dispatch({ type: 'layout', layout: s.layout === 'board' ? 'list' : 'board' });
      if (onItem) ui.list.focusSelected();
      return;
    }
    case 'filter': {
      const first = ui.filters.first();
      // Nothing recorded a facet yet: the palette says so rather than the key doing nothing.
      return first ? first.focus() : palette.open('filter');
    }
    case 'clearFilters':
      return store.dispatch({ type: 'clearFilters' });
    case 'saveView':
      return views.open(null, ui.suggestName(s));
    case 'density': {
      const density: Density = s.density === 'compact' ? 'comfortable' : 'compact';
      store_(DENSITY_KEY, density);
      return store.dispatch({ type: 'density', density });
    }
  }
};

const ui = shell(
  document.getElementById('app')!,
  {
    open: openItem,
    select: (id) => {
      if (store.get().selected !== id) store.dispatch({ type: 'select', id });
    },
    closeItem,
    clearSearch: () => {
      store.dispatch({ type: 'query', query: '' });
      ui.search.focus();
    },
    clearFilters: () => {
      store.dispatch({ type: 'clearFilters' });
      ui.search.focus();
    },
    layout: (layout) => store.dispatch({ type: 'layout', layout }),
    filter: (facet, value) => store.dispatch({ type: 'filter', facet, value }),
    saveView: () => run('saveView'),
    openView: (search) => store.dispatch({ type: 'view', search }),
    editView: (name) => views.open(name),
    query: (text) => store.dispatch({ type: 'query', query: text }),
    palette: () => run('palette'),
    help: () => run('help'),
  },
  mac,
);
const palette = paletteView(
  store.get,
  { run, openItem, filter: (facet, value) => store.dispatch({ type: 'filter', facet, value }), openView: (search) => store.dispatch({ type: 'view', search }) },
  mac,
);
const help = helpView(mac);
const setSaved = (saved: SavedView[]) => {
  store_(VIEWS_KEY, JSON.stringify(saved));
  store.dispatch({ type: 'saved', saved });
};
const views = viewDialog({
  submit(name, editing) {
    const { saved } = store.get();
    const taken = (n: string) => saved.some((v) => v.name !== editing && v.name.toLowerCase() === n.trim().toLowerCase());
    if (taken(name)) return `A view is already called “${name.trim()}”. Pick another name, or edit that view.`;
    const next = editing === null ? saveView(saved, name, store.get()) : renameView(saved, editing, name);
    if (!next) return 'Give the view a name.';
    setSaved(next);
    return null;
  },
  remove: (name) => setSaved(deleteView(store.get().saved, name)),
});
document.body.append(palette.el, help.el, views.el);

// Browsers rate-limit replaceState, and typing a search would hit the limit: write the
// address at most every 250 ms, always ending on the latest state.
const ADDRESS_MS = 250;
let addressAt = 0;
let addressTimer: ReturnType<typeof setTimeout> | undefined;
const writeAddress = () => {
  addressTimer = undefined;
  addressAt = performance.now();
  const next = toAddress(location.search, store.get());
  if (next !== location.search) history.replaceState(null, '', `${location.pathname}${next}${location.hash}`);
};
const paint = (s: ViewState) => {
  document.documentElement.dataset.theme = s.theme;
  document.documentElement.dataset.density = s.density;
  ui.render(s);
  if (addressTimer !== undefined) return;
  const wait = addressAt + ADDRESS_MS - performance.now();
  if (wait <= 0) writeAddress();
  else addressTimer = setTimeout(writeAddress, wait);
};
store.subscribe(paint);
paint(initial);

document.addEventListener('keydown', (e) => {
  // Open dialogs handle their own keys; the native <dialog> closes itself on Escape.
  if (palette.isOpen() || help.isOpen() || views.isOpen() || e.defaultPrevented || e.isComposing) return;
  const t = e.target as HTMLElement;
  // A select owns its arrows and letters; Escape and Mod chords still work there.
  if (t instanceof HTMLSelectElement && e.key !== 'Escape' && !(mac ? e.metaKey : e.ctrlKey)) return;
  const inText = t instanceof HTMLInputElement || t instanceof HTMLTextAreaElement || t.isContentEditable;
  const id = commandFor({ key: e.key, metaKey: e.metaKey, ctrlKey: e.ctrlKey, altKey: e.altKey, inText }, mac);
  if (!id) return;
  // Enter on a focused link or button is that control's own click.
  if (id === 'open' && (inText || t instanceof HTMLAnchorElement || t instanceof HTMLButtonElement)) return;
  e.preventDefault();
  run(id);
});

async function main() {
  const url = q.get('gateway') ?? 'ws://127.0.0.1:8787/ws';
  const room = q.get('room');
  if (!room || !token) return;
  await init();
  const fold = new Folder();
  const sink: Fold = {
    apply: (o, p) => fold.apply(o, p),
    newer: () => fold.newer(),
    at: () => fold.at(),
    stateJson: () => fold.state_json(),
    itemsJson: () => fold.items_json(),
  };
  // Redraw once per frame however many records arrive.
  let queued = false;
  const redraw = () => {
    if (queued) return;
    queued = true;
    requestAnimationFrame(() => {
      queued = false;
      store.dispatch({ type: 'items', items: items(sink) });
    });
  };
  let cursor = 0;
  let refused = 0;
  let refusedBefore = 0; // refusals from earlier connections; follow counts per connection
  let delay = 1000; // grows on failures, resets once a connection goes live
  for (;;) {
    try {
      store.dispatch({ type: 'connection', connection: { kind: 'loading', what: cursor === 0 ? 'history' : 'reconnecting' } });
      const transport = await connect(url, room, token);
      store.dispatch({ type: 'connection', connection: { kind: 'loading', what: cursor === 0 ? 'history' : 'reconnecting', linked: true } });
      refusedBefore = refused;
      cursor = await follow(transport, sink, {
        stream: q.get('stream') ?? 'events',
        from: cursor, // past refused records too, which the fold does not count
        onApplied: redraw,
        onRefused: (n) => {
          refused = refusedBefore + n;
          if (store.get().connection.kind === 'live') store.dispatch({ type: 'connection', connection: { kind: 'live', refused } });
        },
        onLive: () => {
          delay = 1000;
          store.dispatch({ type: 'items', items: items(sink) });
          store.dispatch({ type: 'connection', connection: { kind: 'live', refused } });
        },
      });
    } catch (e) {
      const msg = (e as Error).message;
      // A rejected or expired token, or a refused request, will not pass on a retry.
      if (e instanceof FollowError || (e instanceof GatewayError && e.fatal)) {
        store.dispatch({ type: 'connection', connection: { kind: 'stopped', reason: msg, advice: adviceFor(e.code) } });
        return;
      }
      store.dispatch({ type: 'connection', connection: { kind: 'retrying', reason: msg, seconds: Math.round(delay / 1000) } });
    }
    await new Promise((r) => setTimeout(r, delay));
    delay = Math.min(delay * 2, 30000);
  }
}

void main();
