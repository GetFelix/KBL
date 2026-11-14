// The panel's "Same files" section: one note per overlapping item, with the
// shared paths and the suggested link. The tracker only suggests; nothing is written.
import { moreFilesText, moreNotesText, type CardOverlaps } from '../overlap.ts';
import { h } from './dom.ts';

export function overlapSection(card: CardOverlaps | undefined): HTMLElement | null {
  if (!card || card.notes.length === 0) return null;
  const more = moreNotesText(card);
  return h('section', { class: 'overlaps', 'aria-labelledby': 'overlaps-title' },
    h('h3', { id: 'overlaps-title', class: 'section-title' }, 'Same files'),
    h('ul', { class: 'overlap-notes' },
      ...card.notes.map((note) => {
        const extra = moreFilesText(note);
        return h('li', { class: 'overlap', 'data-other': note.other },
          h('p', {}, 'Shares files with ', h('span', { class: 'row-id' }, note.other), '.'),
          h('ul', { class: 'overlap-files' }, ...note.files.map((f) => h('li', {}, h('code', {}, f))), extra ? h('li', { class: 'muted' }, extra) : null),
          h('p', { class: 'muted' }, `Suggested: ${note.suggest}.`),
        );
      }),
    ),
    more ? h('p', { class: 'muted overlap-more' }, more) : null,
  );
}
