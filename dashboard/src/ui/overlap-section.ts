// The panel's "Same files" section: one note per overlapping item, with the
// shared paths and the suggested link. The tracker only suggests; nothing is written.
import { cardKey, moreFilesText, moreNotesText, type CardOverlaps } from '../overlap.ts';
import type { ViewState } from '../state.ts';
import { h } from './dom.ts';
import { itemHref } from './item-list.ts';

function overlapSection(
  card: CardOverlaps | undefined,
  query: string,
  open: (id: string) => void,
): HTMLElement | null {
  if (!card || card.notes.length === 0) return null;
  const more = moreNotesText(card);
  return h('section', { class: 'overlaps', 'aria-labelledby': 'overlaps-title' },
    h('h3', { id: 'overlaps-title', class: 'section-title' }, 'Same files'),
    h('ul', { class: 'overlap-notes' },
      ...card.notes.map((note) => {
        const extra = moreFilesText(note);
        return h('li', { class: 'overlap', 'data-other': note.other },
          h('p', {}, 'Shares files with ', h('a', {
            class: 'overlap-link',
            href: itemHref(note.other, query),
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

/** The panel field for "Same files": a redraw key and a renderer, in the panel's field shape. */
export const overlapField = {
  key: (item: { id: string }, s: ViewState): string => `${cardKey(s.overlaps.get(item.id))}|${s.query}`,
  render: (item: { id: string }, s: ViewState, on: { open(id: string): void }): HTMLElement | null =>
    overlapSection(s.overlaps.get(item.id), s.query, on.open),
};
