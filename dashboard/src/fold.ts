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
  /** Null or absent unless the log recorded it: the tier from triage or acceptance. */
  tier?: number | null;
  /** The upstream source, such as `github`, from the item's target or its root's import. */
  source?: string | null;
  /** The repository the work lands in, such as `GetFelix/KBL`. */
  repo?: string | null;
}

export function items(fold: Fold): ItemRow[] {
  return JSON.parse(fold.itemsJson()) as ItemRow[];
}
