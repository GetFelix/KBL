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

/** What a card shows for one overlap: the other item, the shared files, and the suggested link. */
export interface OverlapNote {
  other: string;
  files: string[];
  /** The suggested dependency, written as `<item> waits on <item>`. */
  suggest: string;
}

/** The overlap notes for each item, keyed by id. Both items of a pair get a note. */
export function overlapNotes(fold: Fold): Map<string, OverlapNote[]> {
  const notes = new Map<string, OverlapNote[]>();
  const add = (id: string, note: OverlapNote) => notes.set(id, [...(notes.get(id) ?? []), note]);
  for (const o of JSON.parse(fold.overlapsJson()) as Overlap[]) {
    const suggest = `${o.then} waits on ${o.first}`;
    add(o.first, { other: o.then, files: o.files, suggest });
    add(o.then, { other: o.first, files: o.files, suggest });
  }
  return notes;
}
