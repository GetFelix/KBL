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
  /** The upstream tracker the blocker's tree came from, such as `jira`. */
  source: string | null;
  /** Merged or completed, and the link waits for the release that carries it. */
  awaiting_release: boolean;
}

export interface BlockedRow {
  id: string;
  title: string;
  repo: Repo | null;
  source: string | null;
  waits_on: Blocker[];
}

/** The open items a link holds back, each with what holds it and where that lives. */
export function blocked(fold: Fold): BlockedRow[] {
  return JSON.parse(fold.blockedJson()) as BlockedRow[];
}

/** One blocker as the row it holds shows it. `where` and `release` are empty when there is nothing to say. */
export interface BlockerLine {
  id: string;
  title: string | null;
  /** Where the blocker lives, when that differs from the row: `github:GetFelix/felix-gateway`, or a source such as `jira`. */
  where: string;
  /** True when the blocker is merged and only its release is awaited. */
  release: boolean;
}

/**
 * Each blocker of a row: its repo when that differs from the row's repo, else its
 * source when the blocker has no repo and the row's source is another.
 */
export function blockerLines(row: BlockedRow): BlockerLine[] {
  const own = row.repo;
  return row.waits_on.map((b) => {
    const where = b.repo
      ? own && b.repo.source === own.source && b.repo.repo === own.repo
        ? ''
        : `${b.repo.source}:${b.repo.repo}`
      : b.source && b.source !== row.source
        ? b.source
        : '';
    return { id: b.id, title: b.title, where, release: b.awaiting_release };
  });
}

/** What follows a blocker's id: `(github:GetFelix/felix-gateway)`, `(jira) release`, or nothing. */
export function blockerNote(b: BlockerLine): string {
  return [b.where && `(${b.where})`, b.release && 'release'].filter(Boolean).join(' ');
}

/** A blocker on one line: `gw (github:GetFelix/felix-gateway)`, `j1 (jira)`, `p1 release`. */
export function blockerText(b: BlockerLine): string {
  const note = blockerNote(b);
  return note ? `${b.id} ${note}` : b.id;
}

/** What a row waits on, as the list shows it after "waits on". */
export function waitsOn(row: BlockedRow): string {
  return blockerLines(row).map(blockerText).join(', ');
}
