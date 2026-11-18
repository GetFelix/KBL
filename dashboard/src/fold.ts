/** What the page needs from the WebAssembly fold. `Folder` from the wasm build satisfies it. */
export interface Fold {
  /** False when the payload or offset was refused; the state is then unchanged. */
  apply(offset: number, payload: Uint8Array): boolean;
  /** The format of the record that stopped the fold because it is newer than this build reads, else 0. */
  newer(): number;
  /** The log's format so far. */
  format(): number;
  at(): number;
  stateJson(): string;
  itemsJson(): string;
  blockedJson(): string;
  overlapsJson(): string;
  /** What happened on one item, oldest first: the events its timeline and history are built from. */
  activityJson(item: string): string;
  /** What each item's latest finished attempt handed off, keyed by item id. */
  handoffsJson(): string;
  /** The story of every item with a recorded decision, keyed by item id. */
  storiesJson(): string;
  /** `{load, in_review}`: the review load the tracker's pause counts, and the In review part of it. */
  reviewLoadJson(): string;
  /** The fold's spend: what claims cost per root, reviewer tier and repo. */
  spendJson(): string;
}

export interface ItemRow {
  id: string;
  title: string;
  stage: string;
  /** Null or absent unless the log recorded it: the tier from acceptance or a tier change. */
  tier?: number | null;
  /** The tier triage proposed, which an accept without a tier takes. */
  proposed_tier?: number | null;
  /** The upstream source, such as `github`, from the item's target or its root's import. */
  source?: string | null;
  /** The repository the work lands in, such as `GetFelix/KBL`. */
  repo?: string | null;
  /** Why the item waits on a person (`question`, `check_failed`, ...), or null. */
  needs?: string | null;
  /** The offset of its open question, with the options and default that question offers. */
  question?: number | null;
  options?: string[] | null;
  default?: string | null;
  /** What triage could not settle (`scope`, `repo`, `acceptance`) when the open question is triage's. */
  unclear?: string | null;
  /** The stage of a root's triage child, or null when it has none. */
  triage?: string | null;
  /** The ingest rule that admitted a root with no triage item, or null. */
  triage_skipped?: string | null;
  /** True for an item with no parent. */
  root?: boolean;
  /** A person accepted it. Only an accepted item can be flagged. */
  accepted?: boolean;
  /** A person flagged it for a person, whatever else it waits on. */
  flagged?: boolean;
  /** What a person blocked it on, or null. A block with `until_ms` in the past no longer holds. */
  block?: { at_ms: number; by: string; until_ms?: number | null; resource?: string | null; reason: string } | null;
  /** Who the item is assigned to, or absent. An agent assignee is not accepted yet. */
  assignee?: { person: string } | { agent: string } | null;
  /** The attempt an agent holds, when the item is held. */
  held_attempt?: number | null;
  /** The offset of the review finding this item came from, when a review made it. */
  finding?: number | null;
  /** What triage proposed besides the tier, for a person to accept or adjust. */
  proposal?: { why: string; depends_on: string[]; split: string[] } | null;
  /** The agent that holds it and when it last wrote, in the tracker's clock (ms). */
  holder?: string | null;
  active_ms?: number | null;
  /** When the agent's claim runs out, in the tracker's clock (ms). */
  hold_deadline_ms?: number | null;
  /** The item this one is a child of, and what kind of work it is (`fix`, `triage`, `augment`, `review`, ...). */
  parent?: string | null;
  work?: string | null;
  /** The upstream id of the item's root, such as `GetFelix/felix#1694`. */
  upstream?: string | null;
  /** The pull request it reports, and what upstream shows of it. */
  pr?: string | null;
  pr_phase?: string | null;
  pr_checks?: string | null;
  failing_check?: string | null;
  /** The log's format when the page read it, the same for every row. A log below 7 refuses flags. */
  format?: number;
}

/** The inline text of a `question` record, so the panel can show what was asked. An artifact's text is not in the record. */
export function questionText(payload: Uint8Array): string | null {
  try {
    const r = JSON.parse(new TextDecoder().decode(payload)) as { kind?: unknown; text?: { text?: unknown } };
    return r.kind === 'question' && typeof r.text?.text === 'string' ? r.text.text : null;
  } catch {
    return null;
  }
}

export function items(fold: Fold): ItemRow[] {
  const format = fold.format();
  return (JSON.parse(fold.itemsJson()) as ItemRow[]).map((i) => ({ ...i, format }));
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

/** What the review limit counts: In review plus the fixes agents hold, from the same fold as `items`. */
export interface ReviewLoad {
  load: number;
  inReview: number;
}

export function reviewLoad(fold: Pick<Fold, 'reviewLoadJson'>): ReviewLoad {
  const r = JSON.parse(fold.reviewLoadJson()) as { load: number; in_review: number };
  return { load: r.load, inReview: r.in_review };
}
