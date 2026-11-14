// The board: one column per stage in board order, one card per item. Each column is
// one Tab stop (roving tabindex): Tab moves between columns, j/k and the up and down
// arrows move within one, h/l and the side arrows move across. Cards are links in a
// list under the column's heading, so a screen reader hears "Ready, list, 3 items".
import { visible, type Group } from '../list.ts';
import type { ViewState } from '../state.ts';
import { h } from './dom.ts';
import type { ListHandlers } from './item-list.ts';
import { itemHref } from './item-list.ts';

export function boardView(on: ListHandlers) {
  const el = h('div', { class: 'board' });
  const cards = new Map<string, HTMLAnchorElement>();
  let shown = '';
  let restoring = false;
  el.addEventListener('focusin', (e) => {
    const id = (e.target as HTMLElement).closest<HTMLElement>('.card')?.dataset.id;
    if (id && !restoring) on.select(id);
  });
  const card = (id: string, title: string): HTMLAnchorElement => {
    let a = cards.get(id);
    if (!a) {
      a = h('a', {
        class: 'card',
        'data-id': id,
        onclick: (e) => {
          const m = e as MouseEvent;
          if (m.metaKey || m.ctrlKey || m.shiftKey || m.button !== 0) return;
          e.preventDefault();
          on.open(id);
        },
      }, h('span', { class: 'row-id' }, id), h('span', { class: 'card-title' }));
      cards.set(id, a);
    }
    const t = a.lastElementChild!;
    if (t.textContent !== title) t.textContent = title;
    return a;
  };
  return {
    el,
    /** Draws the columns; the caller has already ruled out the empty and error states. */
    render(s: ViewState, columns: Group[] = visible(s.items, s.query, s.filters)) {
      const key = JSON.stringify(columns.map((g) => [g.stage, g.items.map((i) => [i.id, i.title])]));
      if (key !== shown) {
        const active = document.activeElement;
        const focusedId = active instanceof HTMLElement && el.contains(active) ? active.dataset.id : undefined;
        const kept = new Map(cards);
        shown = key;
        cards.clear();
        for (const g of columns) for (const i of g.items) {
          const a = kept.get(i.id);
          if (a) cards.set(i.id, a);
        }
        el.replaceChildren(
          ...columns.map((g) => {
            const headingId = `column-${g.stage}`;
            return h('section', { class: 'column', 'aria-labelledby': headingId },
              h('h3', { id: headingId, class: 'group-title' }, g.label, h('span', { class: 'count' }, String(g.items.length))),
              h('ul', {}, ...g.items.map((i) => h('li', {}, card(i.id, i.title)))),
            );
          }),
        );
        const back = focusedId ? cards.get(focusedId) : undefined;
        if (back) {
          restoring = true;
          back.focus({ preventScroll: true });
          restoring = false;
        }
      }
      // One Tab stop per column: the selected card in its column, else the column's first card.
      for (const g of columns) {
        const stop = g.items.find((i) => i.id === s.selected)?.id ?? g.items[0]?.id;
        for (const i of g.items) cards.get(i.id)?.setAttribute('tabindex', i.id === stop ? '0' : '-1');
      }
      for (const [id, a] of cards) {
        const href = itemHref(id, s);
        if (a.getAttribute('href') !== href) a.setAttribute('href', href);
        a.toggleAttribute('data-selected', id === s.selected);
        if (id === s.open) a.setAttribute('aria-current', 'true');
        else a.removeAttribute('aria-current');
      }
    },
    focusSelected() {
      const selected = el.querySelector<HTMLElement>('[data-selected]');
      selected?.focus();
      selected?.scrollIntoView({ block: 'nearest', inline: 'nearest' });
    },
  };
}
