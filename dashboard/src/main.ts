// Connect to felix-gateway, fold work.events with the core crate as WebAssembly,
// and show the items. Settings come from the query string:
// ?gateway=ws://127.0.0.1:8787/ws&room=kbl&token=<ID token>&stream=events
// This file owns the side effects: the gateway loop, the address and the keys.
// The views under ui/ only render the store.
import './ui/tokens.css';
import './ui/app.css';
import init, { Folder } from './wasm/kbl_wasm.js';
import { trackWaiting } from './inbox.ts';
import { blocked, items, questionText, reviewLoad, type Fold } from './fold.ts';
import { overlapNotes } from './overlap.ts';
import { activities, type Activities } from './activity.ts';
import { handoffs } from './handoff.ts';
import { stories } from './story.ts';
import { priceAgeDays, priceRequest, spend, type PriceEntry, type PriceUnit } from './spend.ts';
import { follow, FollowError } from './follow.ts';
import { connect, GatewayError } from './gateway.ts';
import { DEFAULT_GATEWAY, gatewayHost, pickSession } from './gateway-url.ts';
import { commandFor, type CommandId } from './keys.ts';
import { boardEdge, flatten, markedShown, move, moveBoard, shownRows, visible } from './list.ts';
import { adviceFor, createStore, fromAddress, toAddress, type Density, type View, type ViewState } from './state.ts';
import { deleteView, parseSaved, renameView, saveView, type SavedView } from './views.ts';
import { helpView } from './ui/help-view.ts';
import { paletteView } from './ui/palette-view.ts';
import { shell } from './ui/shell.ts';
import { metrics } from './metrics.ts';
import { metricsView } from './ui/metrics-view.ts';
import { spendView } from './ui/spend-view.ts';
import { pricesView } from './ui/prices-view.ts';
import { viewDialog } from './ui/view-dialog.ts';
import { assignDialog, confirmDialog, signInDialog, type ConfirmKind } from './ui/write-dialogs.ts';
import { forgetGateway, forgetPerson, loadGateway, loadPerson, saveGateway, savePerson } from './session.ts';
import { Tracker } from './tracker.ts';
import { assigneePerson, assignTarget, CAN, canUndo, marksAfter, jobsFor, NO_WRITES, undoRequest, type Job, type WriteArgs, type WriteKind } from './writes.ts';

const mac = /Mac|iPhone|iPad/.test(navigator.platform);
const q = new URLSearchParams(location.search);
// The token never stays in the address bar, history or a shared link. This tab keeps the
// session in sessionStorage so a reload does not lose it (see session.ts).
// A new link is a new session: the earlier person sign-in does not carry over to it.
const pick = pickSession(q, loadGateway());
if (pick.kind === 'session' && pick.fresh) {
  forgetPerson();
  saveGateway({ gateway: pick.gateway, room: pick.room, token: pick.token });
}
const refusal = pick.kind === 'refused' ? pick.reason : null;
const token = pick.kind === 'session' ? pick.token : null;
const roomName = pick.kind === 'session' ? pick.room : null;
const gatewayUrl = pick.kind === 'session' ? pick.gateway : null;
if (q.get('token')) {
  q.delete('token');
  history.replaceState(null, '', `${location.pathname}${q.size ? `?${q}` : ''}${location.hash}`);
}
// Saved views and density are per viewer, so they live in this browser. Storage can
// be missing or throw (private windows, blocked site data); the page works without it.
const VIEWS_KEY = 'kbl.dashboard.views';
const DENSITY_KEY = 'kbl.dashboard.density';
const CHILDREN_KEY = 'kbl.dashboard.children';
// Undefined when storage cannot be read at all, null when the key is not set.
const stored = (key: string): string | null | undefined => {
  try {
    return localStorage.getItem(key);
  } catch {
    return undefined;
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
  connection: refusal ? { kind: 'stopped', reason: refusal, advice: null } : roomName && token ? { kind: 'loading', what: 'history' } : { kind: 'setup' },
  gateway: roomName && token ? gatewayHost(gatewayUrl) : undefined,
  synced: false,
  items: [],
  blocked: [],
  overlaps: new Map(),
  stories: new Map(),
  reviewLoad: { load: 0, inReview: 0 },
  ...address,
  selected: address.open,
  density: stored(DENSITY_KEY) === 'compact' ? 'compact' : 'comfortable',
  childCards: stored(CHILDREN_KEY) === 'cards',
  expanded: [],
  saved: parseSaved(stored(VIEWS_KEY) ?? null),
  marked: [],
  writes: NO_WRITES,
  tracker: { kind: 'signed-out' },
  questions: new Map(),
  kind: null,
  waiting: new Map(),
  theme: themeParam === 'light' || themeParam === 'dark' ? themeParam : matchMedia('(prefers-color-scheme: light)').matches ? 'light' : 'dark',
};
const store = createStore(initial);

// The viewer's tracker token lives here only: never in the address, storage or the store.
let tracker: Tracker | null = null;
const team = q.get('team');
let batches = 0;
let writeIds = 0;

/** What an action applies to: the marked items, else the selected one. */
const targets = (): string[] => {
  const s = store.get();
  const marked = markedShown(s);
  if (marked.length) return marked.map((i) => i.id);
  // As open and mark: a selection the view no longer shows is not acted on.
  return s.selected && shownRows(s).some((r) => r.id === s.selected) ? [s.selected] : [];
};

/**
 * Sends one write per item, in order, each as its own record. One at a time so a bulk
 * action never takes more of the team's in-flight slots than one person clicking.
 */
async function send(kind: WriteKind, jobs: Job[]) {
  const batch = ++batches;
  const list = jobs.map((j) => ({ id: ++writeIds, batch, kind, item: j.item, status: 'pending' as const, undo: j.undo, noUndo: j.noUndo }));
  for (const w of list) store.dispatch({ type: 'write', write: w });
  for (const [i, j] of jobs.entries()) {
    const w = list[i];
    if (typeof j.req === 'string') {
      store.dispatch({ type: 'write', write: { ...w, status: 'refused', reason: j.req, why: undefined, undo: undefined } });
      continue;
    }
    const t = tracker;
    const o = t ? await t.write(j.req) : { status: 'failed' as const, reason: 'Not signed in to the tracker.' };
    if (o.status === 'ok') store.dispatch({ type: 'write', write: { ...w, status: 'ok' } });
    else {
      store.dispatch({ type: 'write', write: { ...w, status: o.status, reason: o.reason, why: o.status === 'refused' ? o.why : undefined, undo: undefined } });
      if (o.status === 'refused' && o.signedOut && t === tracker) {
        tracker = null;
        forgetPerson();
        store.dispatch({ type: 'tracker', tracker: { kind: 'refused', reason: o.reason } });
      }
    }
  }
}

const write = (ids: string[], args: WriteArgs) => {
  const s = store.get();
  // Checked against the log as it is now: a confirm dialog may have been open while someone else acted.
  const jobs = jobsFor(ids, args, s.items, s.writes);
  if (!jobs.length) return;
  // Marks are done with once their action is sent, as in GitHub.
  // Marks on items the action did not apply to stay, so the rest of the selection is still there to act on.
  const rest = marksAfter(s.marked, ids);
  if (rest.length !== s.marked.length) store.dispatch({ type: 'marks', ids: rest });
  void send(args.kind, jobs);
};

const undoLast = () => {
  const s = store.get();
  const batch = s.writes.list.filter((w) => w.batch === s.writes.last);
  if (!canUndo(batch, s.items)) return;
  const done = batch.filter((w) => w.status === 'ok' && w.undo);
  void send('undo', done.map((w) => ({ item: w.item, req: undoRequest(w.undo!, s.items) })));
};

const signIn = async (personToken: string): Promise<string | null> => {
  const before = store.get().tracker;
  store.dispatch({ type: 'tracker', tracker: { kind: 'checking' } });
  const t = new Tracker((url, init) => fetch(url, init), personToken, team);
  const me = await t.me();
  if ('person' in me) {
    const reason =
      !team && me.teams.length > 1
        ? `You are in ${me.teams.length} teams; open the page with ?team= naming one (${me.teams.join(', ')}).`
        : team && !me.teams.includes(team)
          ? `You are not in the team ${team}; open the page with ?team= naming one of yours (${me.teams.join(', ')}).`
          : null;
    if (reason) {
      store.dispatch({ type: 'tracker', tracker: tracker && before.kind === 'signed-in' ? before : { kind: 'refused', reason } });
      return reason;
    }
    tracker = t;
    savePerson(personToken);
    store.dispatch({ type: 'tracker', tracker: { kind: 'signed-in', person: me.person, reviewLimit: me.limits[team ?? me.teams[0] ?? ''] ?? 0 } });
    return null;
  }
  const reason = me.status === 'refused' ? `${me.reason} (${me.why})` : me.status === 'failed' ? me.reason : 'The tracker answered without a person.';
  // A refused token is not kept for the next reload; a bad second token leaves a working sign-in as it was.
  if (me.status === 'refused') forgetPerson();
  store.dispatch({ type: 'tracker', tracker: tracker && before.kind === 'signed-in' ? before : { kind: 'refused', reason } });
  return reason;
};

// Drops the token from memory and from this tab's storage. A write still in flight finishes; the next one is not sent.
const signOut = () => {
  tracker = null;
  forgetPerson();
  store.dispatch({ type: 'tracker', tracker: { kind: 'signed-out' } });
};

const confirmKind = (kind: ConfirmKind, ids: string[]) => {
  const s = store.get();
  if (s.tracker.kind !== 'signed-in') return signInBox.open();
  const list = s.items.filter((i) => ids.includes(i.id) && CAN[kind](i));
  if (list.length) confirm.open(kind, list);
};

const assignItem = (id: string) => {
  const s = store.get();
  const item = assignTarget(s.items, id, s.tracker.kind === 'signed-in');
  if (item === 'sign-in') return signInBox.open();
  if (!item || s.tracker.kind !== 'signed-in') return;
  const names = [...new Set(s.items.flatMap((i) => assigneePerson(i) ?? []))].sort();
  // The picker opens at once; the agents fill in when the tracker answers.
  const agents = tracker ? tracker.agents().then((a) => (Array.isArray(a) ? a : [])) : Promise.resolve([]);
  assignBox.open(item, s.tracker.person, names, agents);
};

const openItem = (id: string) => {
  store.dispatch({ type: 'open', id });
  ui.panel.focus();
};
const closeItem = () => {
  const before = store.get();
  store.dispatch({ type: 'open', id: null });
  // The open item may have no visible row (filtered out, or not in the log): keep the focus somewhere real.
  const rows = shownRows(before);
  if (before.open && rows.some((r) => r.id === before.open)) return ui.list.focusSelected();
  // A nested child has no row; the selection is already on its visible root.
  if (before.selected && rows.some((r) => r.id === before.selected)) return ui.list.focusSelected();
  const first = rows[0]?.id;
  if (first) {
    store.dispatch({ type: 'select', id: first });
    ui.list.focusSelected();
  } else ui.search.focus();
};

const showView = (view: View) => {
  store.dispatch({ type: 'view', view });
  ui.main.focus();
};

const run = (id: CommandId): void => {
  const s = store.get();
  switch (id) {
    case 'palette':
      return palette.open();
    case 'help':
      return help.open();
    case 'spend':
      return showSpend();
    case 'metrics':
      return metricsBox.open();
    case 'prices':
      return showPrices();
    case 'search':
      ui.search.focus();
      return ui.search.select();
    case 'next':
    case 'previous':
    case 'left':
    case 'right':
    case 'first':
    case 'last': {
      const groups = visible(s);
      const rows = flatten(groups);
      const board = s.layout === 'board';
      if (!board && (id === 'left' || id === 'right')) return;
      const dir = ({ next: 'down', previous: 'up', left: 'left', right: 'right' } as const)[id as 'next'];
      const target =
        id === 'first' || id === 'last'
          ? board
            ? boardEdge(groups, s.selected, id)
            : ((id === 'first' ? rows[0] : rows.at(-1))?.id ?? null)
          : board
              ? moveBoard(groups, s.selected, dir)
              : move(rows, s.selected, id === 'next' ? 1 : -1);
      store.dispatch({ type: 'select', id: target });
      return ui.list.focusSelected();
    }
    case 'open':
      if (s.selected && shownRows(s).some((r) => r.id === s.selected)) openItem(s.selected);
      return;
    case 'close':
      if (ui.menuOpen()) return ui.closeMenu();
      if (document.activeElement === ui.search) ui.search.blur();
      if (s.open) closeItem();
      else if (s.query) store.dispatch({ type: 'query', query: '' });
      else if (s.marked.length) store.dispatch({ type: 'marks', ids: [] });
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
    case 'groupBy':
      return ui.filters.focusGroup();
    case 'collapseGroups':
    case 'expandGroups':
      return store.dispatch({ type: 'collapseAll', collapsed: id === 'collapseGroups' });
    case 'saveView':
      return views.open(null, ui.suggestName(s));
    case 'density': {
      const density: Density = s.density === 'compact' ? 'comfortable' : 'compact';
      store_(DENSITY_KEY, density);
      return store.dispatch({ type: 'density', density });
    }
    case 'children':
      store_(CHILDREN_KEY, s.childCards ? 'nested' : 'cards');
      return store.dispatch({ type: 'childCards', on: !s.childCards });
    case 'view-all':
    case 'view-inbox':
    case 'view-blocked':
      return showView(id === 'view-all' ? 'all' : id === 'view-inbox' ? 'inbox' : 'blocked');
    case 'mark':
      if (s.selected && shownRows(s).some((r) => r.id === s.selected)) store.dispatch({ type: 'mark', id: s.selected });
      return;
    case 'markAll':
      return store.dispatch({ type: 'marks', ids: shownRows(s).map((r) => r.id) });
    case 'clearMarks':
      return store.dispatch({ type: 'marks', ids: [] });
    case 'accept':
      return confirmKind('accept', targets());
    case 'closeItems':
      return confirmKind('close', targets());
    case 'flag':
    case 'unflag': {
      if (s.tracker.kind !== 'signed-in') return signInBox.open();
      const picked = targets();
      const ids = s.items.filter((i) => picked.includes(i.id) && CAN[id](i)).map((i) => i.id);
      return ids.length ? write(ids, { kind: id }) : undefined;
    }
    case 'assign': {
      const ids = targets();
      return ids.length === 1 ? assignItem(ids[0]) : undefined;
    }
    case 'undo':
      return undoLast();
    case 'signIn':
      return signInBox.open();
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
    children: () => run('children'),
    toggle: (id) => store.dispatch({ type: 'expand', id }),
    filter: (facet, value) => store.dispatch({ type: 'filter', facet, value }),
    group: (group) => store.dispatch({ type: 'group', group }),
    toggleGroup: (key) => store.dispatch({ type: 'collapse', key }),
    saveView: () => run('saveView'),
    openView: (search) => store.dispatch({ type: 'openView', search }),
    editView: (name) => views.open(name),
    query: (text) => store.dispatch({ type: 'query', query: text }),
    palette: () => run('palette'),
    help: () => run('help'),
    spend: () => run('spend'),
    metrics: () => run('metrics'),
    mark: (id) => store.dispatch({ type: 'mark', id }),
    signIn: () => signInBox.open(),
    signOut,
    confirm: confirmKind,
    setTier: (ids, tier) => write(ids, { kind: 'tier', tier }),
    acceptNow: (ids, tier) => write(ids, { kind: 'accept', tier }),
    advice: (item, proposal, depends_on, split) => write([item], { kind: 'advice', depends_on, split, proposal }),
    kind: (kind) => store.dispatch({ type: 'kind', kind }),
    flag: (ids, flagged) => write(ids, { kind: flagged ? 'flag' : 'unflag' }),
    take: (item) => write([item], { kind: 'take' }),
    assign: assignItem,
    steer: (item, attempt, text) => write([item], { kind: 'steer', attempt, text }),
    rate: (item, attempt, accurate) => write([item], { kind: 'rate', attempt, accurate }),
    answer: (item, question, text, chosen) => write([item], { kind: 'answer', question, text, chosen }),
    undo: undoLast,
    dismiss: () => {
      ui.writes.dismiss(store.get().writes.last);
      paint(store.get());
    },
    clearMarks: () => {
      store.dispatch({ type: 'marks', ids: [] });
      ui.main.focus();
    },
  },
  mac,
);
const palette = paletteView(
  store.get,
  { run, openItem, filter: (facet, value) => store.dispatch({ type: 'filter', facet, value }), group: (group) => store.dispatch({ type: 'group', group }), openView: (search) => store.dispatch({ type: 'openView', search }) },
  mac,
);
const help = helpView(mac);
// Set once the fold exists; Spend does nothing before then.
let readSpend: (() => ReturnType<typeof spend>) | null = null;
let readMetrics: ((days: number) => ReturnType<typeof metrics>) | null = null;
const setPrice = async (model: string, unit: PriceUnit, dollarsText: string, from = '', replaced: PriceEntry | null = null): Promise<string | null> => {
  const req = priceRequest(model, unit, dollarsText, from, Date.now(), replaced);
  if (typeof req === 'string') return req;
  const o = tracker ? await tracker.write(req) : ({ status: 'failed', reason: 'Not signed in to the tracker.' } as const);
  if (o.status === 'refused' && o.why === 'price_changed') return 'Price changed, review and retry.';
  return o.status === 'ok' ? null : o.reason;
};
const spendBox = spendView({
  open: openItem,
  canSetPrice: () => tracker !== null,
  setPrice,
  openPrices: () => showPrices(),
  now: () => Date.now(),
  priceAgeDays: priceAgeDays(q.get('price_age_days')),
});
const metricsBox = metricsView({
  report: (days) => (store.get().connection.kind === 'live' && readMetrics ? readMetrics(days) : null),
  signedIn: () => tracker !== null,
  load: async (days) => {
    const t = tracker;
    const r = t ? await t.summaryTrust(days) : null;
    if (r === null) return 'Not signed in to the tracker.';
    if ('status' in r) return r.status === 'ok' ? 'The tracker answered without numbers.' : r.reason;
    return r;
  },
  signIn: () => signInBox.open(),
});
const pricesBox = pricesView({ canSetPrice: () => tracker !== null, setPrice, now: () => Date.now() });
// Spend of a log still being read would be a partial total, so the dialog waits for live and fills in then.
const showPrices = () => {
  const s = store.get();
  pricesBox.open(s.connection.kind === 'live' && readSpend ? readSpend() : null);
};
const showSpend = () => {
  const s = store.get();
  spendBox.open(s.connection.kind === 'live' && readSpend ? readSpend() : null, s.items, s);
};
// Another tab may have changed the views since this one read them: every write starts
// from what storage holds now, and a change made elsewhere shows up here as it lands.
const currentSaved = (): SavedView[] => {
  const raw = stored(VIEWS_KEY);
  return raw === undefined ? store.get().saved : parseSaved(raw);
};
const setSaved = (saved: SavedView[]) => {
  store_(VIEWS_KEY, JSON.stringify(saved));
  store.dispatch({ type: 'saved', saved });
};
addEventListener('storage', (e) => {
  if (e.key === VIEWS_KEY || e.key === null) store.dispatch({ type: 'saved', saved: currentSaved() });
});
const views = viewDialog({
  submit(name, editing) {
    const saved = currentSaved();
    const next = editing === null ? saveView(saved, name, store.get()) : renameView(saved, editing, name);
    if (typeof next === 'string') return next;
    setSaved(next);
    return null;
  },
  // A renamed view keeps the keyboard on its Edit button; a new one leaves it where Save was opened.
  saved: (name, editing) => {
    if (editing !== null) ui.focusSaved(name, true);
  },
  remove: (name) => {
    const saved = currentSaved();
    const at = saved.findIndex((v) => v.name === name);
    const next = deleteView(saved, name);
    setSaved(next);
    ui.focusSaved(next[at]?.name ?? null, false);
  },
});
const confirm = confirmDialog(write);
const assignBox = assignDialog(write);
const signInBox = signInDialog(signIn);
document.body.append(palette.el, help.el, spendBox.el, metricsBox.el, pricesBox.el, views.el, confirm.el, assignBox.el, signInBox.el);

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

// The open item's events and its children's, read from the fold: when the log moves (see `redraw`)
// and when another item opens.
let readActivity: (() => Activities) | null = null;
let activityFor: string | null = null;
store.subscribe((s) => {
  if (s.open === activityFor) return;
  activityFor = s.open;
  if (readActivity) store.dispatch({ type: 'activity', activity: readActivity() });
});
// "Last active 3 min ago" ages without a record; redraw in place so it keeps up.
setInterval(() => ui.render(store.get()), 30_000);
paint(initial);

document.addEventListener('keydown', (e) => {
  // Open dialogs handle their own keys; the native <dialog> closes itself on Escape.
  if (palette.isOpen() || help.isOpen() || spendBox.isOpen() || metricsBox.isOpen() || pricesBox.isOpen() || views.isOpen() || confirm.isOpen() || assignBox.isOpen() || signInBox.isOpen() || e.defaultPrevented || e.isComposing) return;
  const t = e.target as HTMLElement;
  // A select owns its arrows and letters; Escape and Mod chords still work there.
  if (t instanceof HTMLSelectElement && e.key !== 'Escape' && !(mac ? e.metaKey : e.ctrlKey)) return;
  const inText = t instanceof HTMLInputElement || t instanceof HTMLTextAreaElement || t.isContentEditable;
  const id = commandFor({ key: e.key, metaKey: e.metaKey, ctrlKey: e.ctrlKey, altKey: e.altKey, inText }, mac);
  if (!id) return;
  // Enter on a focused link or button is that control's own click.
  if (id === 'open' && (inText || t instanceof HTMLAnchorElement || t instanceof HTMLButtonElement)) return;
  // In a field, Mod+Z is the field's own undo.
  if (id === 'undo' && inText) return;
  e.preventDefault();
  run(id);
});

function openActivity(fold: Fold): Activities {
  const open = store.get().open;
  if (!open) return new Map();
  return activities(fold, [open, ...items(fold).filter((i) => i.parent === open).map((i) => i.id)]);
}

async function main() {
  const url = gatewayUrl ?? DEFAULT_GATEWAY;
  const room = roomName;
  if (!room || !token) return;
  await init();
  const fold = new Folder();
  const questions = new Map<number, string>();
  // Time spent in the fold, read by the browser check's performance budget (check/browser.mjs).
  let foldMs = 0;
  const sink: Fold = {
    apply: (o, p) => {
      const t0 = performance.now();
      const ok = fold.apply(o, p);
      foldMs += performance.now() - t0;
      const text = ok ? questionText(p) : null;
      if (text !== null) questions.set(o, text);
      return ok;
    },
    newer: () => fold.newer(),
    format: () => fold.format(),
    at: () => fold.at(),
    stateJson: () => fold.state_json(),
    itemsJson: () => fold.items_json(),
    blockedJson: () => fold.blocked_json(),
    overlapsJson: () => fold.overlaps_json(),
    storiesJson: () => fold.stories_json(),
    activityJson: (item) => fold.activity_json(item),
    handoffsJson: () => fold.handoffs_json(),
    reviewLoadJson: () => fold.review_load_json(),
    spendJson: () => fold.spend_json(),
    metricsJson: (nowMs, days) => fold.metrics_json(nowMs, days),
  };
  readSpend = () => spend(sink);
  readMetrics = (days) => metrics(sink, Date.now(), days);
  readActivity = () => openActivity(sink);
  const snapshot = () => {
    const rows = items(sink);
    return { type: 'items' as const, items: rows, blocked: blocked(sink), overlaps: overlapNotes(sink), stories: stories(sink), handoffs: handoffs(sink), reviewLoad: reviewLoad(sink), questions: new Map(questions), waiting: trackWaiting(store.get().waiting, rows, sink.at()), activity: openActivity(sink) };
  };
  // Redraw once per frame however many records arrive.
  let queued = false;
  const redraw = () => {
    if (queued) return;
    queued = true;
    // While the history loads, a redraw rebuilds every row read so far, so a frame per page makes
    // the load quadratic. Twice a second shows progress without that.
    const wait = store.get().connection.kind === 'live' ? 0 : 500;
    setTimeout(() => requestAnimationFrame(() => {
      queued = false;
      store.dispatch(snapshot());
      if (spendBox.isOpen()) showSpend();
      if (readSpend) pricesBox.refresh(readSpend());
    }), wait);
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
          performance.mark('kbl-live', { detail: { foldMs } });
          store.dispatch(snapshot());
          store.dispatch({ type: 'connection', connection: { kind: 'live', refused } });
          if (spendBox.isOpen()) showSpend();
          if (readSpend) pricesBox.refresh(readSpend());
        },
      });
    } catch (e) {
      const msg = (e as Error).message;
      // A rejected or expired token, or a refused request, will not pass on a retry.
      if (e instanceof FollowError || (e instanceof GatewayError && e.fatal)) {
        // The saved session is no use to a reload either; the next one asks for a new link.
        forgetGateway();
        store.dispatch({ type: 'connection', connection: { kind: 'stopped', reason: msg, advice: adviceFor(e.code) } });
        return;
      }
      store.dispatch({ type: 'connection', connection: { kind: 'retrying', reason: msg, seconds: Math.round(delay / 1000) } });
    }
    await new Promise((r) => setTimeout(r, delay));
    delay = Math.min(delay * 2, 30000);
  }
}

// A reload in this tab signs back in with the person token it kept.
const savedPerson = loadPerson();
if (savedPerson) void signIn(savedPerson);
void main();
