/** What the page needs from the WebAssembly fold. `Folder` from the wasm build satisfies it. */
export interface Fold {
  /** False when the payload or offset was refused; the state is then unchanged. */
  apply(offset: number, payload: Uint8Array): boolean;
  /** The format of the record that stopped the fold because it is newer than this build reads, else 0. */
  newer(): number;
  at(): number;
  stateJson(): string;
  itemsJson(): string;
  overlapsJson(): string;
}

export interface ItemRow {
  id: string;
  title: string;
  stage: string;
}

export function items(fold: Fold): ItemRow[] {
  return JSON.parse(fold.itemsJson()) as ItemRow[];
}

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

/** The overlap notes for each item, keyed by id. Both items of a pair get a note. */
export function overlapNotes(fold: Fold): Map<string, CardOverlaps> {
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
