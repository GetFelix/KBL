// The panel's "Same files" section: one note per overlapping item, with the
// shared paths and the suggested link. The tracker only suggests; nothing is written.
import { cardKey, moreFilesText, moreNotesText, type CardOverlaps } from '../overlap.ts';
import type { ViewState } from '../state.ts';
import { h } from './dom.ts';
import { itemHref } from './item-list.ts';
import type { Field } from './item-panel.ts';

function overlapSection(
  card: CardOverlaps | undefined,
  s: ViewState,
  open: (id: string) => void,
): Node | null {
  if (!card || card.notes.length === 0) return null;
  const more = moreNotesText(card);
  return h('section', { class: 'overlaps', 'aria-label': 'Shared files' },
    h('ul', { class: 'overlap-notes' },
      ...card.notes.map((note) => {
        const extra = moreFilesText(note);
        return h('li', { class: 'overlap', 'data-other': note.other },
          h('p', {}, 'Shares files with ', h('a', {
            class: 'overlap-link',
            'data-item': note.other,
            href: itemHref(note.other, s),
            'data-focus': `other:${note.other}`,
            onclick: (e) => {
              const m = e as MouseEvent;
              if (m.metaKey || m.ctrlKey || m.shiftKey || m.button !== 0) return;
              e.preventDefault();
              open(note.other);
            },
          }, note.other), '.'),
          h('ul', { class: 'overlap-files' }, ...note.files.map((f) => h('li', {}, h('code', {}, f))), extra ? h('li', { class: 'muted' }, extra) : null),
          h('p', { class: 'muted' }, `Suggested: ${note.suggest}.`),
        );
      }),
    ),
    more ? h('p', { class: 'muted overlap-more' }, more) : null,
  );
}

/** The panel field for "Same files". Its links follow the view and query in place (see item-panel). */
export const overlapField: Field = {
  key: (item, s) => cardKey(s.overlaps.get(item.id)),
  render: (item, s, on) => {
    const section = overlapSection(s.overlaps.get(item.id), s, on.open);
    return section ? ['Same files', section] : null;
  },
};
