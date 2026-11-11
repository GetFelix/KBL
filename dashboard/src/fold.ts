/** What the page needs from the WebAssembly fold. `Folder` from the wasm build satisfies it. */
export interface Fold {
  /** False when the payload or offset was refused; the state is then unchanged. */
  apply(offset: number, payload: Uint8Array): boolean;
  at(): number;
  stateJson(): string;
  itemsJson(): string;
}

export interface ItemRow {
  id: string;
  title: string;
  stage: string;
}

export function items(fold: Fold): ItemRow[] {
  return JSON.parse(fold.itemsJson()) as ItemRow[];
}
