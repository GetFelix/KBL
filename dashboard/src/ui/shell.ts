// The page layout: skip link, sidebar, top bar with search and status, the list
// and the side panel. Landmarks (nav, main, aside) let a screen reader jump between them.
import { chordFor, displayChord } from '../keys.ts';
import { toAddress, type ViewState } from '../state.ts';
import { decodeView, sameView, summary, type SavedView } from '../views.ts';
import { boardView } from './board-view.ts';
import { h, kbd } from './dom.ts';
import { filterBar, type FilterHandlers } from './filter-bar.ts';
import { itemList, type ListHandlers } from './item-list.ts';
import { itemPanel } from './item-panel.ts';
import { statusBar } from './status-bar.ts';

export interface ShellHandlers extends ListHandlers, FilterHandlers {
  query(text: string): void;
  /** Opens a saved view by its query string. */
  openView(search: string): void;
  editView(name: string): void;
  closeItem(): void;
  palette(): void;
  help(): void;
}

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
  const board = boardView(on);
  const list = itemList(on, board);
  const filters = filterBar(on, mac);
  const panel = itemPanel(on.closeItem);
  const status = statusBar();
  const search = h('input', {
    id: 'search',
    type: 'search',
    'aria-label': 'Search items by id or title',
    placeholder: 'Search',
    autocomplete: 'off',
    oninput: (e) => on.query((e.target as HTMLInputElement).value),
  });
  const allCount = h('span', { class: 'count muted' });
  const allLink = h('a', {
    href: '?',
    onclick: (e) => {
      e.preventDefault();
      setMenu(false);
      on.openView('');
    },
  }, 'All work', allCount);
  const savedList = h('ul', { class: 'nav saved', 'aria-label': 'Saved views' });
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
  const setMenu = (open: boolean) => {
    const was = menu.getAttribute('aria-expanded') === 'true';
    if (was === open) return;
    menu.setAttribute('aria-expanded', String(open));
    sidebar.toggleAttribute('data-open', open);
    if (open) sidebar.querySelector<HTMLElement>('a, button')?.focus();
    else if (sidebar.contains(document.activeElement)) menu.focus();
  };
  // A click outside the open drawer closes it, as in any app's side menu.
  document.addEventListener('click', (e) => {
    if (menu.getAttribute('aria-expanded') === 'true' && !sidebar.contains(e.target as Node) && e.target !== menu) setMenu(false);
  });
  let savedShown = '';
  root.replaceChildren(
    h('a', { class: 'skip', href: '#work' }, 'Skip to the work list'),
    h('div', { class: 'shell' },
      sidebar,
      h('div', { class: 'main' },
        h('header', { class: 'topbar' },
          menu,
          heading,
          h('label', { class: 'search' }, search, kbd(displayChord(chordFor('search'), mac))),
          status.el,
        ),
        h('div', { class: 'content' },
          h('main', { id: 'work', tabindex: '-1', 'aria-labelledby': 'work-title' }, h('h2', { id: 'work-title', class: 'sr-only' }, 'Work list'), filters.el, list.el),
          panel.el,
        ),
      ),
    ),
  );
  sidebar.append(
    h('div', { class: 'brand' }, mark(), 'KBL'),
    h('ul', { class: 'nav' }, h('li', {}, allLink)),
    h('h2', { class: 'nav-title', id: 'saved-title' }, 'Saved views'),
    savedList,
    h('div', { class: 'sidebar-foot' },
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
              h('button', { type: 'button', class: 'button ghost small', 'aria-label': `Edit view ${v.name}`, onclick: () => on.editView(v.name) }, 'Edit'),
            ))
        : [h('li', { class: 'muted nav-empty' }, 'Filter or search, then Save view.')]),
    );
  };

  return {
    search,
    list,
    panel,
    filters,
    /** A starting name for Save view: the active filters and search in words. */
    suggestName: (s: ViewState) => [summary(s.filters), s.query && `“${s.query}”`, s.layout === 'board' ? 'board' : ''].filter(Boolean).join(', '),
    menuOpen: () => menu.getAttribute('aria-expanded') === 'true',
    closeMenu: () => setMenu(false),
    render(s: ViewState) {
      renderSaved(s.saved);
      const current = sameView(s, { layout: 'list', query: '', filters: {} });
      const named = s.saved.find((v) => sameView(s, decodeView(v.search)));
      heading.textContent = named?.name ?? 'All work';
      if (current) allLink.setAttribute('aria-current', 'page');
      else allLink.removeAttribute('aria-current');
      allLink.setAttribute('href', toAddress(location.search, { query: '', open: null, layout: 'list', filters: {} }) || '?');
      for (const a of Array.from(savedList.querySelectorAll<HTMLAnchorElement>('a[data-search]'))) {
        const v = decodeView(a.dataset.search ?? '');
        a.setAttribute('href', toAddress(location.search, { ...v, open: null }) || '?');
        if (a.textContent === named?.name) a.setAttribute('aria-current', 'page');
        else a.removeAttribute('aria-current');
      }
      filters.render(s);
      if (search.value !== s.query) search.value = s.query;
      allCount.textContent = s.items.length ? String(s.items.length) : '';
      status.render(s);
      list.render(s);
      panel.render(s);
    },
  };
}
