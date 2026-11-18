// The page layout: skip link, sidebar, top bar with search and status, the list
// and the side panel. Landmarks (nav, main, aside) let a screen reader jump between them.
import { chordFor, displayChord } from '../keys.ts';
import { decodeView, encodeView, sameView, summary, type SavedView, type ViewSpec } from '../views.ts';
import { inScope } from '../list.ts';
import { reviewFull, toAddress, type View, type ViewState } from '../state.ts';
import { boardView } from './board-view.ts';
import { h, kbd } from './dom.ts';
import { filterBar, type FilterHandlers } from './filter-bar.ts';
import { itemList, type ListHandlers } from './item-list.ts';
import { itemPanel } from './item-panel.ts';
import { bulkBar } from './bulk-bar.ts';
import type { WriteHandlers } from './item-actions.ts';
import { statusBar } from './status-bar.ts';

export interface ShellHandlers extends ListHandlers, FilterHandlers, WriteHandlers {
  query(text: string): void;
  /** Opens a saved view by its query string. */
  openView(search: string): void;
  editView(name: string): void;
  closeItem(): void;
  palette(): void;
  help(): void;
  spend(): void;
  metrics(): void;
  /** Forgets the tracker token in this tab. */
  signOut(): void;
}

const VIEWS: readonly [View, string][] = [
  ['all', 'All work'],
  ['inbox', 'Needs you'],
  ['blocked', 'Blocked'],
];

const plain = (view: View): ViewSpec => ({ view, layout: 'list', query: '', filters: {} });

const MARK = 'M8 12H36V28H52V56H8Z';

function mark(): SVGSVGElement {
  const ns = 'http://www.w3.org/2000/svg';
  const svg = document.createElementNS(ns, 'svg');
  svg.setAttribute('viewBox', '0 0 64 64');
  svg.setAttribute('aria-hidden', 'true');
  const body = document.createElementNS(ns, 'path');
  body.setAttribute('d', MARK);
  body.setAttribute('class', 'brand-mark');
  const bite = document.createElementNS(ns, 'rect');
  for (const [k, v] of Object.entries({ x: '40', y: '8', width: '16', height: '16', class: 'brand-bite' })) bite.setAttribute(k, v);
  svg.append(body, bite);
  return svg;
}

export function shell(root: HTMLElement, on: ShellHandlers, mac: boolean) {
  // Polite and separate from the connection status, so a moved or vanished item is said once.
  const announcer = h('p', { class: 'sr-only announce', 'aria-live': 'polite' });
  const announce = (text: string) => {
    announcer.textContent = text;
  };
  const board = boardView(on, announce);
  const list = itemList(on, board, announce);
  const filters = filterBar(on, mac);
  const panel = itemPanel(on);
  const writes = bulkBar(on);
  const status = statusBar();
  const full = h('p', { class: 'review-full', role: 'status', 'aria-live': 'polite' });
  full.hidden = true;
  const search = h('input', {
    id: 'search',
    type: 'search',
    'aria-label': 'Search items by id or title',
    placeholder: 'Search',
    autocomplete: 'off',
    oninput: (e) => on.query((e.target as HTMLInputElement).value),
  });
  // One sidebar link per view, each opening it plain: no filters or search, as a list.
  // The palette's Show commands change only the view, keeping the rest. A modified click opens a tab.
  const nav = VIEWS.map(([view, label]) => {
    const search = encodeView('', plain(view));
    const count = h('span', { class: 'count muted' });
    const a = h('a', {
      'data-view': view,
      onclick: (e) => {
        const m = e as MouseEvent;
        if (m.metaKey || m.ctrlKey || m.shiftKey || m.button !== 0) return;
        e.preventDefault();
        setMenu(false);
        on.openView(search);
      },
    }, label, count);
    return { view, label, a, count };
  });
  const savedList = h('ul', { class: 'nav saved', 'aria-label': 'Saved views' });
  // Who the writes are made as, and the way to stop: one button, so the focus stays on it either way.
  const who = h('p', { class: 'account-who muted small' });
  let signedIn = false;
  // The link reads the board; acting needs the person token, a separate sign-in.
  const hint = h('p', { class: 'account-hint muted small' }, 'Your link only reads. Signing in with your person token lets you accept, reject, assign and close work.');
  const account = h('button', { type: 'button', class: 'button ghost account-button', onclick: () => (setMenu(false), signedIn ? on.signOut() : on.signIn()) }, 'Sign in to act');
  const heading = h('h1', {}, 'All work');
  // Below 720 px the sidebar is a drawer behind this button, so Commands and
  // Shortcuts stay in the same place on every screen (WCAG 3.2.6).
  const menu = h('button', {
    type: 'button',
    class: 'button ghost menu-button',
    'aria-expanded': 'false',
    'aria-controls': 'sidebar',
    onclick: () => setMenu(menu.getAttribute('aria-expanded') !== 'true'),
  }, 'Menu');
  const sidebar = h('nav', { id: 'sidebar', class: 'sidebar', 'aria-label': 'Views' });
  const backdrop = h('div', { class: 'drawer-backdrop', hidden: true });
  const skip = h('a', { class: 'skip', href: '#work' }, 'Skip to the work list');
  const page = h('div', { class: 'main' });
  // While the drawer is open the page behind it is inert, as behind a modal: Tab stays
  // in the drawer and a screen reader does not wander into the list underneath.
  const setMenu = (open: boolean) => {
    const was = menu.getAttribute('aria-expanded') === 'true';
    if (was === open) return;
    menu.setAttribute('aria-expanded', String(open));
    sidebar.toggleAttribute('data-open', open);
    backdrop.hidden = !open;
    page.inert = open;
    skip.inert = open;
    if (open) sidebar.querySelector<HTMLElement>('a, button')?.focus();
    else if (sidebar.contains(document.activeElement) || document.activeElement === document.body) menu.focus();
  };
  // Tab wraps inside the open drawer instead of leaving for the browser's own controls.
  sidebar.addEventListener('keydown', (e) => {
    if (e.key !== 'Tab' || !sidebar.hasAttribute('data-open')) return;
    const stops = Array.from(sidebar.querySelectorAll<HTMLElement>('a[href], button')).filter((x) => x.checkVisibility());
    const [first, end] = [stops[0], stops.at(-1)];
    if (e.shiftKey ? document.activeElement === first : document.activeElement === end) {
      e.preventDefault();
      (e.shiftKey ? end : first)?.focus();
    }
  });
  // Widening the window turns the drawer back into the sidebar; nothing may stay inert.
  matchMedia('(max-width: 720px)').addEventListener('change', (e) => {
    if (!e.matches) setMenu(false);
  });
  // A click outside the open drawer closes it, as in any app's side menu.
  document.addEventListener('click', (e) => {
    if (menu.getAttribute('aria-expanded') === 'true' && !sidebar.contains(e.target as Node) && e.target !== menu) setMenu(false);
  });
  const main = h('main', { id: 'work', tabindex: '-1', 'aria-labelledby': 'work-title' }, h('h2', { id: 'work-title', class: 'sr-only' }, 'Work list'), filters.el, writes.el, list.el, announcer);
  let savedShown = '';
  page.append(
    h('header', { class: 'topbar' },
      menu,
      heading,
      h('label', { class: 'search' }, search, kbd(displayChord(chordFor('search'), mac))),
      full,
      status.el,
    ),
    h('div', { class: 'content' },
      main,
      panel.el,
    ),
  );
  root.replaceChildren(skip, h('div', { class: 'shell' }, sidebar, backdrop, page));
  sidebar.append(
    h('div', { class: 'brand' }, mark(), 'KBL'),
    h('ul', { class: 'nav' }, ...nav.map((n) => h('li', {}, n.a))),
    h('h2', { class: 'nav-title', id: 'saved-title' }, 'Saved views'),
    savedList,
    h('div', { class: 'sidebar-foot' },
      h('div', { class: 'account' }, who, hint, account),
      h('button', { type: 'button', class: 'button ghost', onclick: () => (setMenu(false), on.spend()) }, 'Spend'),
      h('button', { type: 'button', class: 'button ghost', onclick: () => (setMenu(false), on.metrics()) }, 'Metrics'),
      h('button', { type: 'button', class: 'button ghost', onclick: () => (setMenu(false), on.palette()) }, 'Commands', kbd(displayChord(chordFor('palette'), mac))),
      h('button', { type: 'button', class: 'button ghost', onclick: () => (setMenu(false), on.help()) }, 'Shortcuts', kbd(displayChord(chordFor('help'), mac))),
    ),
  );
  savedList.setAttribute('aria-labelledby', 'saved-title');
  savedList.removeAttribute('aria-label');

  const renderSaved = (saved: SavedView[]) => {
    const key = JSON.stringify(saved);
    if (key === savedShown) return;
    savedShown = key;
    savedList.replaceChildren(
      ...(saved.length
        ? saved.map((v) =>
            h('li', { class: 'saved-row' },
              h('a', {
                'data-search': v.search,
                onclick: (e) => {
                  const m = e as MouseEvent;
                  if (m.metaKey || m.ctrlKey || m.shiftKey) return;
                  e.preventDefault();
                  setMenu(false);
                  on.openView(v.search);
                },
              }, v.name),
              // The drawer closes first, so the dialog is the only thing open and Cancel returns to Menu.
              h('button', { type: 'button', class: 'button ghost small', 'aria-label': `Edit view ${v.name}`, onclick: () => (setMenu(false), on.editView(v.name)) }, 'Edit'),
            ))
        : [h('li', { class: 'muted nav-empty' }, 'Filter or search, then Save view.')]),
    );
  };

  return {
    writes,
    search,
    list,
    panel,
    filters,
    /** A starting name for Save view: the active filters and search in words. */
    suggestName: (s: ViewState) => [s.view === 'blocked' ? 'Blocked' : s.view === 'inbox' ? 'Needs you' : '', summary(s.filters), s.query && `“${s.query}”`, s.layout === 'board' ? 'board' : ''].filter(Boolean).join(', '),
    menuOpen: () => menu.getAttribute('aria-expanded') === 'true',
    /**
     * After a saved-view change: focus `name`'s Edit button (`edit`) or its link, else Save
     * view. In the closed phone drawer the row is not showing, so Menu takes it instead.
     */
    focusSaved(name: string | null, edit: boolean) {
      const row = Array.from(savedList.querySelectorAll<HTMLElement>('.saved-row')).find((r) => r.querySelector('a')?.textContent === name);
      const target = row?.querySelector<HTMLElement>(edit ? 'button' : 'a');
      if (!target) return filters.focusSave();
      if (target.checkVisibility()) return target.focus();
      menu.focus();
    },
    closeMenu: () => setMenu(false),
    main,
    render(s: ViewState) {
      renderSaved(s.saved);
      signedIn = s.tracker.kind === 'signed-in';
      const whoText = s.tracker.kind === 'signed-in' ? `Signed in as ${s.tracker.person}` : s.tracker.kind === 'checking' ? 'Checking the token…' : 'Viewing (read-only)';
      if (who.textContent !== whoText) who.textContent = whoText;
      const label = signedIn ? 'Sign out' : 'Sign in to act';
      hint.hidden = signedIn || s.tracker.kind === 'checking';
      if (account.textContent !== label) account.textContent = label;
      const named = s.saved.find((v) => sameView(s, decodeView(v.search)));
      for (const n of nav) {
        const total = inScope({ ...s, view: n.view }).length;
        n.count.textContent = total ? String(total) : '';
        n.a.setAttribute('href', toAddress(location.search, { ...plain(n.view), open: null }) || '?');
        if (!named && sameView(s, plain(n.view))) n.a.setAttribute('aria-current', 'page');
        else n.a.removeAttribute('aria-current');
      }
      const title = named?.name ?? VIEWS.find(([v]) => v === s.view)?.[1] ?? 'All work';
      if (heading.textContent !== title) heading.textContent = title;
      for (const a of Array.from(savedList.querySelectorAll<HTMLAnchorElement>('a[data-search]'))) {
        const v = decodeView(a.dataset.search ?? '');
        a.setAttribute('href', toAddress(location.search, { ...v, open: null }) || '?');
        if (a.textContent === named?.name) a.setAttribute('aria-current', 'page');
        else a.removeAttribute('aria-current');
      }
      filters.render(s);
      writes.render(s);
      if (search.value !== s.query) search.value = s.query;
      status.render(s);
      const line = s.tracker.kind === 'signed-in' ? reviewFull(s.reviewLoad, s.tracker.reviewLimit) : null;
      full.hidden = !line;
      if (line && full.textContent !== `▲${line}`) full.replaceChildren(h('span', { class: 'status-icon', 'aria-hidden': 'true' }, '▲'), line);
      list.render(s);
      panel.render(s);
    },
  };
}
