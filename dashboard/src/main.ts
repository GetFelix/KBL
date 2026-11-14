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
import { flatten, group, move } from './list.ts';
import { createStore, fromAddress, toAddress, type ViewState } from './state.ts';
import { helpView } from './ui/help-view.ts';
import { paletteView } from './ui/palette-view.ts';
import { shell } from './ui/shell.ts';

const mac = /Mac|iPhone|iPad/.test(navigator.platform);
const q = new URLSearchParams(location.search);
const token = q.get('token');
// Keep the token in memory only: out of the address bar, history and any shared link.
if (token) {
  q.delete('token');
  history.replaceState(null, '', `${location.pathname}${q.size ? `?${q}` : ''}${location.hash}`);
}
const themeParam = q.get('theme');
const address = fromAddress(location.search);
const initial: ViewState = {
  connection: { kind: 'setup' },
  items: [],
  ...address,
  selected: address.open,
  theme: themeParam === 'light' || themeParam === 'dark' ? themeParam : matchMedia('(prefers-color-scheme: light)').matches ? 'light' : 'dark',
};
const store = createStore(initial);

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
    case 'first':
    case 'last': {
      const rows = flatten(group(s.items, s.query));
      const target =
        id === 'first' ? (rows[0]?.id ?? null) : id === 'last' ? (rows.at(-1)?.id ?? null) : move(rows, s.selected, id === 'next' ? 1 : -1);
      store.dispatch({ type: 'select', id: target });
      return ui.list.focusSelected();
    }
    case 'open':
      if (s.selected) store.dispatch({ type: 'open', id: s.selected });
      return;
    case 'close':
      if (s.open) store.dispatch({ type: 'open', id: null });
      else if (s.query) store.dispatch({ type: 'query', query: '' });
      if (document.activeElement === ui.search) ui.search.blur();
      return;
    case 'theme':
      return store.dispatch({ type: 'theme', theme: s.theme === 'dark' ? 'light' : 'dark' });
  }
};

const ui = shell(
  document.getElementById('app')!,
  {
    open: (id) => store.dispatch({ type: 'open', id }),
    closeItem: () => store.dispatch({ type: 'open', id: null }),
    clearSearch: () => {
      store.dispatch({ type: 'query', query: '' });
      ui.search.focus();
    },
    query: (text) => store.dispatch({ type: 'query', query: text }),
    palette: () => run('palette'),
    help: () => run('help'),
  },
  mac,
);
const palette = paletteView(store.get, { run, openItem: (id) => store.dispatch({ type: 'open', id }) }, mac);
const help = helpView(mac);
document.body.append(palette.el, help.el);

const paint = (s: ViewState) => {
  document.documentElement.dataset.theme = s.theme;
  ui.render(s);
  const next = toAddress(location.search, s);
  if (next !== location.search) history.replaceState(null, '', `${location.pathname}${next}${location.hash}`);
};
store.subscribe(paint);
paint(initial);

document.addEventListener('keydown', (e) => {
  // Open dialogs handle their own keys; the native <dialog> closes itself on Escape.
  if (palette.isOpen() || help.isOpen() || e.defaultPrevented || e.isComposing) return;
  const t = e.target as HTMLElement;
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
  store.dispatch({ type: 'connection', connection: { kind: 'loading', what: 'history' } });
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
      if (cursor !== 0) store.dispatch({ type: 'connection', connection: { kind: 'loading', what: 'reconnecting' } });
      const transport = await connect(url, room, token);
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
        store.dispatch({ type: 'connection', connection: { kind: 'stopped', reason: msg } });
        return;
      }
      store.dispatch({ type: 'connection', connection: { kind: 'retrying', reason: msg, seconds: Math.round(delay / 1000) } });
    }
    await new Promise((r) => setTimeout(r, delay));
    delay = Math.min(delay * 2, 30000);
  }
}

void main();
