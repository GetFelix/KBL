// The panel's "Waits on" field: what holds the open item back, each blocker a link to it.
import { blockerLines, blockerNote } from '../fold.ts';
import { h } from './dom.ts';
import { itemHref } from './item-list.ts';
import type { Field } from './item-panel.ts';

export const waitsOnField: Field = {
  key: (item, s) => JSON.stringify(s.blocked.find((b) => b.id === item.id) ?? null),
  render(item, s, on) {
    const row = s.blocked.find((b) => b.id === item.id);
    if (!row) return null;
    // A person can walk the chain from the panel. The href follows the query in place (see item-panel).
    const lines = blockerLines(row).map((b) =>
      h('li', {},
        h('a', {
          class: 'blocker-id',
          'data-item': b.id,
          href: itemHref(b.id, s),
          onclick: (e) => {
            const m = e as MouseEvent;
            if (m.metaKey || m.ctrlKey || m.shiftKey || m.button !== 0) return;
            e.preventDefault();
            on.open(b.id);
          },
        }, b.id),
        b.title ? h('span', {}, ` ${b.title}`) : null,
        blockerNote(b) ? h('span', { class: 'muted' }, ` ${blockerNote(b)}`) : null,
      ),
    );
    return ['Waits on', h('ul', { class: 'blockers' }, ...lines)];
  },
};
