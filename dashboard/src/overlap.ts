// File overlaps between open items, shaped for the panel and the row flag.
// See docs/work-tracker.md "Files that overlap". No DOM here.
import type { Fold } from './fold.ts';

/** Two open items whose `files` entries share paths. `then` is suggested to wait on `first`. */
export interface Overlap {
  first: string;
  then: string;
  files: string[];
}

/** At most this many overlap notes on one card, and this many paths in one note. */
export const MAX_NOTES = 5;
export const MAX_FILES = 10;

/** What a card shows for one overlap: the other item, the shared files, and the suggested link. */
export interface OverlapNote {
  other: string;
  /** The first `MAX_FILES` shared paths. */
  files: string[];
  /** How many shared paths `files` leaves out. */
  moreFiles: number;
  /** The suggested dependency, written as `<item> waits on <item>`. */
  suggest: string;
}

/** The overlap notes on one card, at most `MAX_NOTES` of them. */
export interface CardOverlaps {
  notes: OverlapNote[];
  /** How many notes `notes` leaves out. */
  more: number;
}

/** Overlap notes keyed by item id. An item with no overlaps has no entry. */
export type Overlaps = ReadonlyMap<string, CardOverlaps>;

/** The overlap notes for each item. Both items of a pair get a note. */
export function overlapNotes(fold: Pick<Fold, 'overlapsJson'>): Map<string, CardOverlaps> {
  const cards = new Map<string, CardOverlaps>();
  const add = (id: string, note: OverlapNote) => {
    const card = cards.get(id) ?? { notes: [], more: 0 };
    if (card.notes.length < MAX_NOTES) card.notes.push(note);
    else card.more++;
    cards.set(id, card);
  };
  for (const o of JSON.parse(fold.overlapsJson()) as Overlap[]) {
    const suggest = `${o.then} waits on ${o.first}`;
    const files = o.files.slice(0, MAX_FILES);
    const moreFiles = o.files.length - files.length;
    add(o.first, { other: o.then, files, moreFiles, suggest });
    add(o.then, { other: o.first, files, moreFiles, suggest });
  }
  return cards;
}

/** How many items a card overlaps with, counting the notes it leaves out. */
export function overlapCount(card: CardOverlaps | undefined): number {
  return card ? card.notes.length + card.more : 0;
}

/** The row flag's text, or '' for an item with no overlaps. */
export function flagText(card: CardOverlaps | undefined): string {
  const n = overlapCount(card);
  return n === 0 ? '' : n === 1 ? '1 file overlap' : `${n} file overlaps`;
}

/** The trailing "+N more" for a note's file list, or ''. */
export function moreFilesText(note: OverlapNote): string {
  return note.moreFiles ? `+${note.moreFiles} more` : '';
}

/** The line under the notes for the ones a card leaves out, or ''. */
export function moreNotesText(card: CardOverlaps): string {
  return card.more ? `+${card.more} more ${card.more === 1 ? 'overlap' : 'overlaps'}` : '';
}

/** A stable key for what the panel shows, so a redraw is skipped when nothing changed. */
export function cardKey(card: CardOverlaps | undefined): string {
  return card ? JSON.stringify(card) : '';
}
