// The list view: items grouped by stage, one link per row, or the state that
// explains why there are none. Rows are links, so Tab, Enter and middle-click work
// natively; j and k move the selection and the focus together.
import { visible, type Group } from '../list.ts';
import { toAddress, type AddressState, type ViewState } from '../state.ts';
import { activeCount } from '../views.ts';
import { h } from './dom.ts';

export interface ListHandlers {
  open(id: string): void;
  /** Focus landed on a row: it becomes the selection, so keys act on what has focus. */
  select(id: string): void;
  clearSearch(): void;
  clearFilters(): void;
}

const SKELETON_ROWS = 6;

function emptyState(title: string, body: Node | string, action?: HTMLElement): HTMLElement {
  return h('div', { class: 'empty' }, h('h2', {}, title), h('p', {}, body), action);
}

function code(text: string): HTMLElement {
  return h('code', {}, text);
}

const ADVICE = {
  'sign-in': 'A new sign-in link fixes a rejected or expired token. Reload once you have one.',
  reload: 'The log has records newer than this page reads. Reload to get the current page.',
} as const;

// Built from the store's query, not the address bar, which lags the store by up to 250 ms.
export function itemHref(id: string, s: AddressState): string {
  return toAddress(location.search, { ...s, open: id }) || '?';
}

// Rows are kept by item id and reused, and the list is only rebuilt when what it shows
// changes. A live record must not take the focus off the row a keyboard user is on.
export function itemList(
  on: ListHandlers,
  board: { el: HTMLElement; render(s: ViewState, columns: Group[]): void; focusSelected(): void },
): { el: HTMLElement; render(s: ViewState): void; focusSelected(): void } {
  const el = h('div', { class: 'list' });
  const rows = new Map<string, HTMLAnchorElement>();
  let shown = '';
  let restoring = false;
  el.addEventListener('focusin', (e) => {
    const id = (e.target as HTMLElement).closest<HTMLElement>('.row')?.dataset.id;
    if (id && !restoring) on.select(id);
  });

  const show = (key: string, ...children: HTMLElement[]) => {
    if (key === shown) return;
    shown = key;
    rows.clear();
    el.replaceChildren(...children);
  };
  const row = (id: string, title: string): HTMLAnchorElement => {
    let a = rows.get(id);
    if (!a) {
      a = h('a', {
        class: 'row',
        'data-id': id,
        onclick: (e) => {
          const m = e as MouseEvent;
          if (m.metaKey || m.ctrlKey || m.shiftKey || m.button !== 0) return;
          e.preventDefault();
          on.open(id);
        },
      }, h('span', { class: 'row-id' }, id), h('span', { class: 'row-title' }));
      rows.set(id, a);
    }
    const t = a.lastElementChild!;
    if (t.textContent !== title) t.textContent = title;
    return a;
  };

  // The board keeps its own cards; the list only decides that it is the board's turn.
  const showBoard = (s: ViewState, columns: Group[]) => {
    if (shown !== 'board') {
      shown = 'board';
      rows.clear();
      el.replaceChildren(board.el);
    }
    board.render(s, columns);
  };
  const view = {
    el,
    render(s: ViewState) {
      const c = s.connection;
      // After a failed first attempt the retry view stays up until the socket is joined.
      const again = c.kind === 'loading' && !c.linked && (shown.startsWith('retrying') || shown === 'again');
      el.toggleAttribute('aria-busy', !s.synced && s.items.length === 0 && c.kind === 'loading' && !again);
      if (c.kind === 'setup') {
        return show('setup',
          emptyState(
            'Connect to a gateway',
            h('span', {}, 'Open this page with ', code('?gateway=wss://…&room=…&token=…'), ' in the address. The dashboard README shows the scope the gateway needs.'),
          ),
        );
      }
      if (c.kind === 'stopped') {
        // Rebuilt only when the stop changes, so the alert is announced once.
        const advice = c.advice ? ADVICE[c.advice] : null;
        return show(`stopped|${c.reason}|${c.advice}`,
          h('div', { class: 'empty error', role: 'alert' },
            h('h2', {}, 'The dashboard stopped'),
            h('p', {}, c.reason),
            advice ? h('p', {}, advice) : null,
            advice ? h('button', { type: 'button', class: 'button', onclick: () => location.reload() }, 'Reload') : null,
          ),
        );
      }
      if (!s.synced && s.items.length === 0) {
        if (c.kind === 'retrying') {
          return show(`retrying|${c.reason}|${c.seconds}`,
            emptyState('Cannot reach the gateway', `Nothing has been read yet. ${c.reason}. Trying again in ${c.seconds} s.`));
        }
        // A retry after a failed first attempt keeps saying so instead of flashing placeholder rows.
        if (again) {
          return show('again', emptyState('Cannot reach the gateway', 'Nothing has been read yet. Trying again now.'));
        }
        return show('loading',
          h('ul', { class: 'skeleton', 'aria-hidden': 'true' }, ...Array.from({ length: SKELETON_ROWS }, () => h('li', {}, h('span'), h('span')))),
        );
      }
      if (s.items.length === 0) return show('empty', emptyState('No work yet', 'Items appear here as the tracker imports issues from its sources.'));
      const groups = visible(s.items, s.query, s.filters);
      if (groups.length === 0) {
        const filtered = activeCount(s.filters) > 0;
        const what = s.query ? `“${s.query}”` : 'these filters';
        return show(`nomatch|${s.query}|${JSON.stringify(s.filters)}`,
          emptyState(`Nothing matches ${what}`, filtered ? 'Search looks at item ids and titles, inside the filters.' : 'Search looks at item ids and titles.',
            h('span', { class: 'actions' },
              s.query ? h('button', { type: 'button', class: 'button', onclick: on.clearSearch }, 'Clear search') : null,
              filtered ? h('button', { type: 'button', class: 'button', onclick: on.clearFilters }, 'Clear filters') : null,
            )),
        );
      }
      if (s.layout === 'board') return showBoard(s, groups);
      const key = `list|${JSON.stringify(groups.map((g) => [g.stage, g.items.map((i) => [i.id, i.title])]))}`;
      if (key !== shown) {
        const active = document.activeElement;
        const focusedId = active instanceof HTMLElement && el.contains(active) ? active.dataset.id : undefined;
        const kept = new Map(rows);
        shown = key;
        rows.clear();
        for (const g of groups) for (const i of g.items) {
          const a = kept.get(i.id);
          if (a) rows.set(i.id, a);
        }
        el.replaceChildren(
          ...groups.map((g) => {
            const headingId = `group-${g.stage}`;
            return h('section', { class: 'group', 'aria-labelledby': headingId },
              h('h2', { id: headingId, class: 'group-title' }, g.label, h('span', { class: 'count' }, String(g.items.length))),
              h('ul', {}, ...g.items.map((item) => h('li', {}, row(item.id, item.title)))),
            );
          }),
        );
        // Moving a focused node blurs it; put the focus back without scrolling or re-selecting.
        const back = focusedId ? rows.get(focusedId) : undefined;
        if (back) {
          restoring = true;
          back.focus({ preventScroll: true });
          restoring = false;
        }
      }
      for (const [id, a] of rows) {
        const href = itemHref(id, s);
        if (a.getAttribute('href') !== href) a.setAttribute('href', href);
        a.toggleAttribute('data-selected', id === s.selected);
        if (id === s.open) a.setAttribute('aria-current', 'true');
        else a.removeAttribute('aria-current');
      }
    },
    focusSelected() {
      if (shown === 'board') return board.focusSelected();
      const selected = el.querySelector<HTMLElement>('[data-selected]');
      selected?.focus();
      selected?.scrollIntoView({ block: 'nearest' });
    },
  };
  return view;
}
