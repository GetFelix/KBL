/** What the page needs from the WebAssembly fold. `Folder` from the wasm build satisfies it. */
export interface Fold {
  /** False when the payload or offset was refused; the state is then unchanged. */
  apply(offset: number, payload: Uint8Array): boolean;
  /** The format of the record that stopped the fold because it is newer than this build reads, else 0. */
  newer(): number;
  at(): number;
  stateJson(): string;
  itemsJson(): string;
  blockedJson(): string;
}

export interface ItemRow {
  id: string;
  title: string;
  stage: string;
}

export function items(fold: Fold): ItemRow[] {
  return JSON.parse(fold.itemsJson()) as ItemRow[];
}

/** Where an item's work lands, such as `github` and `GetFelix/felix-gateway`. */
export interface Repo {
  source: string;
  repo: string;
}

export interface Blocker {
  id: string;
  title: string | null;
  repo: Repo | null;
  stage: string | null;
}

export interface BlockedRow {
  id: string;
  title: string;
  repo: Repo | null;
  waits_on: Blocker[];
}

/** The open items a link holds back, each with what holds it and where that lives. */
export function blocked(fold: Fold): BlockedRow[] {
  return JSON.parse(fold.blockedJson()) as BlockedRow[];
}

/** How a blocker reads on the row it holds: its id, plus its repo when that is not the row's own. */
export function waitsOn(row: BlockedRow): string {
  const own = row.repo;
  return row.waits_on
    .map((b) =>
      b.repo && !(own && b.repo.source === own.source && b.repo.repo === own.repo)
        ? `${b.id} (${b.repo.source}:${b.repo.repo})`
        : b.id,
    )
    .join(', ');
}
