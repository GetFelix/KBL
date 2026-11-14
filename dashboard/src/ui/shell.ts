// The page layout: skip link, sidebar, top bar with search and status, the list
// and the side panel. Landmarks (nav, main, aside) let a screen reader jump between them.
import { chordFor, displayChord } from '../keys.ts';
import { toAddress, type View, type ViewState } from '../state.ts';
import { h, kbd } from './dom.ts';
import { itemList, type ListHandlers } from './item-list.ts';
import { itemPanel } from './item-panel.ts';
import { statusBar } from './status-bar.ts';

export interface ShellHandlers extends ListHandlers {
  query(text: string): void;
  closeItem(): void;
  palette(): void;
  help(): void;
  view(view: View): void;
}

const VIEWS: readonly [View, string][] = [
  ['all', 'All work'],
  ['blocked', 'Blocked'],
];

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
  const list = itemList(on);
  const panel = itemPanel(on);
  const status = statusBar();
  const search = h('input', {
    id: 'search',
    type: 'search',
    'aria-label': 'Search items by id or title',
    placeholder: 'Search',
    autocomplete: 'off',
    oninput: (e) => on.query((e.target as HTMLInputElement).value),
  });
  // One sidebar link per view; a plain click switches in place, a modified click opens a tab.
  const nav = VIEWS.map(([view, label]) => {
    const count = h('span', { class: 'count muted' });
    const a = h('a', {
      'data-view': view,
      onclick: (e) => {
        const m = e as MouseEvent;
        if (m.metaKey || m.ctrlKey || m.shiftKey || m.button !== 0) return;
        e.preventDefault();
        on.view(view);
      },
    }, label, count);
    return { view, label, a, count };
  });
  const title = h('h1', {}, 'All work');
  const main = h('main', { id: 'work', tabindex: '-1', 'aria-labelledby': 'work-title' }, h('h2', { id: 'work-title', class: 'sr-only' }, 'Work list'), list.el);
  root.replaceChildren(
    h('a', { class: 'skip', href: '#work' }, 'Skip to the work list'),
    h('div', { class: 'shell' },
      h('nav', { class: 'sidebar', 'aria-label': 'Views' },
        h('div', { class: 'brand' }, mark(), 'KBL'),
        h('ul', { class: 'nav' }, ...nav.map((n) => h('li', {}, n.a))),
        h('div', { class: 'sidebar-foot' },
          h('button', { type: 'button', class: 'button ghost', onclick: on.palette }, 'Commands', kbd(displayChord(chordFor('palette'), mac))),
          h('button', { type: 'button', class: 'button ghost', onclick: on.help }, 'Shortcuts', kbd(displayChord(chordFor('help'), mac))),
        ),
      ),
      h('div', { class: 'main' },
        h('header', { class: 'topbar' },
          title,
          h('label', { class: 'search' }, search, kbd(displayChord(chordFor('search'), mac))),
          status.el,
        ),
        h('div', { class: 'content' },
          main,
          panel.el,
        ),
      ),
    ),
  );
  return {
    search,
    list,
    panel,
    main,
    render(s: ViewState) {
      if (search.value !== s.query) search.value = s.query;
      for (const n of nav) {
        const total = n.view === 'all' ? s.items.length : s.blocked.length;
        n.count.textContent = total ? String(total) : '';
        n.a.setAttribute('href', toAddress(location.search, { view: n.view, query: s.query, open: null }) || '?');
        if (n.view === s.view) n.a.setAttribute('aria-current', 'page');
        else n.a.removeAttribute('aria-current');
        if (n.view === s.view && title.textContent !== n.label) title.textContent = n.label;
      }
      status.render(s);
      list.render(s);
      panel.render(s);
    },
  };
}
