// The page layout: skip link, sidebar, top bar with search and status, the list
// and the side panel. Landmarks (nav, main, aside) let a screen reader jump between them.
import { displayChord } from '../keys.ts';
import type { ViewState } from '../state.ts';
import { h, kbd } from './dom.ts';
import { itemList, type ListHandlers } from './item-list.ts';
import { itemPanel } from './item-panel.ts';
import { statusBar } from './status-bar.ts';

export interface ShellHandlers extends ListHandlers {
  query(text: string): void;
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
  const list = itemList(on);
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
  root.replaceChildren(
    h('a', { class: 'skip', href: '#work' }, 'Skip to the work list'),
    h('div', { class: 'shell' },
      h('nav', { class: 'sidebar', 'aria-label': 'Views' },
        h('div', { class: 'brand' }, mark(), 'KBL'),
        h('ul', { class: 'nav' }, h('li', {}, h('a', { href: '#work', 'aria-current': 'page' }, 'All work', allCount))),
        h('div', { class: 'sidebar-foot' },
          h('button', { type: 'button', class: 'button ghost', onclick: on.palette }, 'Commands', kbd(displayChord('Mod+K', mac))),
          h('button', { type: 'button', class: 'button ghost', onclick: on.help }, 'Shortcuts', kbd(['?'])),
        ),
      ),
      h('div', { class: 'main' },
        h('header', { class: 'topbar' },
          h('h1', {}, 'All work'),
          h('label', { class: 'search' }, search, kbd(['/'])),
          status.el,
        ),
        h('div', { class: 'content' },
          h('main', { id: 'work', tabindex: '-1', 'aria-labelledby': 'work-title' }, h('h2', { id: 'work-title', class: 'sr-only' }, 'Work list'), list.el),
          panel.el,
        ),
      ),
    ),
  );
  return {
    search,
    list,
    render(s: ViewState) {
      if (search.value !== s.query) search.value = s.query;
      allCount.textContent = s.items.length ? String(s.items.length) : '';
      status.render(s);
      list.render(s);
      panel.render(s);
    },
  };
}
