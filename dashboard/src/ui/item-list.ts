// The list view: items grouped by stage, one link per row, or the state that
// explains why there are none. Rows are links, so Tab, Enter and middle-click work
// natively; j and k move the selection and the focus together.
import { group } from '../list.ts';
import type { ViewState } from '../state.ts';
import { h } from './dom.ts';

export interface ListHandlers {
  open(id: string): void;
  clearSearch(): void;
}

const SKELETON_ROWS = 6;

function emptyState(title: string, body: Node | string, action?: HTMLElement): HTMLElement {
  return h('div', { class: 'empty' }, h('h2', {}, title), h('p', {}, body), action);
}

function code(text: string): HTMLElement {
  return h('code', {}, text);
}

export function itemHref(id: string): string {
  const q = new URLSearchParams(location.search);
  q.set('item', id);
  return `?${q}`;
}

export function itemList(on: ListHandlers): { el: HTMLElement; render(s: ViewState): void; focusSelected(): void } {
  const el = h('div', { class: 'list' });
  const view = {
    el,
    render(s: ViewState) {
      const c = s.connection;
      if (c.kind === 'setup') {
        el.replaceChildren(
          emptyState(
            'Connect to a gateway',
            h('span', {}, 'Open this page with ', code('?gateway=wss://…&room=…&token=…'), ' in the address. The dashboard README shows the scope the gateway needs.'),
          ),
        );
        return;
      }
      if (c.kind === 'stopped') {
        el.replaceChildren(
          h('div', { class: 'empty error', role: 'alert' },
            h('h2', {}, 'The dashboard stopped'),
            h('p', {}, c.reason),
            h('p', {}, 'A new sign-in link usually fixes a rejected or expired token. Reload once you have one.'),
            h('button', { type: 'button', class: 'button', onclick: () => location.reload() }, 'Reload'),
          ),
        );
        return;
      }
      if (s.items.length === 0 && c.kind === 'loading') {
        el.replaceChildren(
          h('ul', { class: 'skeleton', 'aria-hidden': 'true' }, ...Array.from({ length: SKELETON_ROWS }, () => h('li', {}, h('span'), h('span')))),
        );
        el.setAttribute('aria-busy', 'true');
        return;
      }
      el.removeAttribute('aria-busy');
      if (s.items.length === 0) {
        el.replaceChildren(emptyState('No work yet', 'Items appear here as the tracker imports issues from its sources.'));
        return;
      }
      const groups = group(s.items, s.query);
      if (groups.length === 0) {
        el.replaceChildren(
          emptyState(`Nothing matches “${s.query}”`, 'Search looks at item ids and titles.',
            h('button', { type: 'button', class: 'button', onclick: on.clearSearch }, 'Clear search')),
        );
        return;
      }
      el.replaceChildren(
        ...groups.map((g) => {
          const headingId = `group-${g.stage}`;
          return h('section', { class: 'group', 'aria-labelledby': headingId },
            h('h2', { id: headingId, class: 'group-title' }, g.label, h('span', { class: 'count' }, String(g.items.length))),
            h('ul', {},
              ...g.items.map((item) =>
                h('li', {},
                  h('a', {
                    class: 'row',
                    href: itemHref(item.id),
                    'data-id': item.id,
                    'aria-current': item.id === s.open ? 'true' : undefined,
                    'data-selected': item.id === s.selected ? 'true' : undefined,
                    onclick: (e) => {
                      const m = e as MouseEvent;
                      if (m.metaKey || m.ctrlKey || m.shiftKey || m.button !== 0) return;
                      e.preventDefault();
                      on.open(item.id);
                    },
                  },
                    h('span', { class: 'row-id' }, item.id),
                    h('span', { class: 'row-title' }, item.title),
                  ),
                ),
              ),
            ),
          );
        }),
      );
    },
    focusSelected() {
      const selected = el.querySelector<HTMLElement>('[data-selected="true"]');
      selected?.focus();
      selected?.scrollIntoView({ block: 'nearest' });
    },
  };
  return view;
}
