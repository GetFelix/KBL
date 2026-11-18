// The board: one column per stage in board order, one card per item. Each column is
// one Tab stop (roving tabindex): Tab moves between columns, j/k and the up and down
// arrows move within one, h/l and the side arrows move across. Cards are links in a
// list under the column's heading, so a screen reader hears "Ready, list, 3 items".
import { lead, childIndex, properties, reasonLine, summarize, workingLine } from '../card.ts';
import { waitsOn } from '../fold.ts';
import { visible, type Group } from '../list.ts';
import type { ViewState } from '../state.ts';
import { flagText } from '../overlap.ts';
import { assigneePerson } from '../writes.ts';
import { paintAssignee } from './assignee-chip.ts';
import { childStrip, focusedStrip, refocusChip, stripKey, type StripHandlers } from './child-strip.ts';
import { paintTriage } from './triage-chip.ts';
import { h } from './dom.ts';
import { drawn, itemHref, restoreFocus, type Announce, type Drawn, type ListHandlers } from './item-list.ts';

export function boardView(on: ListHandlers & StripHandlers, announce: Announce) {
  const el = h('div', { class: 'board' });
  const cards = new Map<string, HTMLAnchorElement>();
  let shown = '';
  let restoring = false;
  let last: Drawn = drawn([]);
  let lastColumns: string[][] = [];
  const quietly = (target: HTMLElement) => {
    restoring = true;
    target.focus({ preventScroll: true });
    restoring = false;
  };
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
      }, h('span', { class: 'row-id card-lead' }, id), h('span', { class: 'card-title' }), h('span', { class: 'card-working', hidden: true }), h('span', { class: 'card-props muted', hidden: true }), h('span', { class: 'card-waits muted', hidden: true }), h('span', { class: 'row-flag card-flag', hidden: true }), h('span', { class: 'triage-chip', hidden: true }), h('span', { class: 'card-mark', hidden: true }, 'Selected'), h('span', { class: 'assignee', hidden: true }));
      cards.set(id, a);
    }
    const t = a.querySelector('.card-title')!;
    if (t.textContent !== title) t.textContent = title;
    return a;
  };
  return {
    el,
    /** Draws the columns; the caller has already ruled out the empty and error states. */
    render(s: ViewState, columns: Group[] = visible(s)) {
      const kids = childIndex(s.items);
      const open = new Set(s.expanded ?? []);
      const strips = (id: string) => stripKey(kids.has(id) ? summarize(kids.get(id)!) : undefined, open.has(id));
      const key = JSON.stringify(columns.map((g) => [g.stage, g.items.map((i) => [i.id, i.title, strips(i.id)])]));
      if (key !== shown) {
        const active = document.activeElement;
        const focusedId = active instanceof HTMLElement && el.contains(active) ? active.dataset.id : undefined;
        const focusedChip = focusedStrip(el);
        const before = last;
        const column = lastColumns.find((c) => focusedId !== undefined && c.includes(focusedId)) ?? [];
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
              h('ul', {}, ...g.items.map((i) => h('li', {}, card(i.id, i.title), kids.has(i.id) && !s.childCards ? childStrip(i.id, summarize(kids.get(i.id)!), open.has(i.id), (id) => itemHref(id, s), on) : null))),
            );
          }),
        );
        last = drawn(columns);
        lastColumns = columns.map((g) => g.items.map((i) => i.id));
        // A card that moved keeps the focus; one that left hands it to a card in its old column first.
        restoreFocus(focusedId, before, last, cards, el, quietly, { select: on.select, announce }, [column, before.order]);
        refocusChip(focusedChip, el);
      }
      // One Tab stop per column: the selected card in its column, else the column's first card.
      for (const g of columns) {
        const stop = g.items.find((i) => i.id === s.selected)?.id ?? g.items[0]?.id;
        for (const i of g.items) cards.get(i.id)?.setAttribute('tabindex', i.id === stop ? '0' : '-1');
      }
      const now = Date.now();
      const waits = new Map(s.blocked.map((b) => [b.id, waitsOn(b)]));
      const byId = new Map(s.items.map((i) => [i.id, i]));
      for (const [id, a] of cards) {
        const href = itemHref(id, s);
        if (a.getAttribute('href') !== href) a.setAttribute('href', href);
        a.toggleAttribute('data-selected', id === s.selected);
        // Marked in text as well as the border, so it does not rest on colour.
        const marked = s.marked.includes(id);
        a.toggleAttribute('data-marked', marked);
        a.querySelector<HTMLElement>('.card-mark')!.hidden = !marked;
        // Same text flag as the list row, so the board says it too.
        const flag = a.querySelector<HTMLElement>('.row-flag')!;
        const text = flagText(s.overlaps.get(id));
        if (flag.textContent !== text) flag.textContent = text;
        flag.hidden = text === '';
        const item = byId.get(id);
        paintAssignee(a, assigneePerson(item ?? {}));
        if (item) {
          const set = (sel: string, text: string) => {
            const n = a.querySelector<HTMLElement>(sel)!;
            if (n.textContent !== text) n.textContent = text;
            n.hidden = text === '';
          };
          set('.card-lead', lead(item));
          set('.card-working', workingLine(item, now) ?? '');
          set('.card-props', properties(item, now).join(' · '));
        }
        paintTriage(a, item);
        // Same "waits on" line as a Blocked row in the list.
        const w = a.querySelector<HTMLElement>('.card-waits')!;
        const line = item ? reasonLine(item, waits.get(id)) : '';
        if (w.textContent !== line) w.textContent = line;
        w.hidden = line === '';
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
