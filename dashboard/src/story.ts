// An item's story as the fold builds it (core/src/story.rs): recorded decisions by
// phase, the edits and evidence behind each, and the flags where a claim and the
// recorded steps disagree. Nothing here is generated. No DOM.
import type { Fold } from './fold.ts';

export type Phase = 'understand' | 'reproduce' | 'cause' | 'fix' | 'verify';

/** Text a record holds inline, or the id of an artifact it lives in. */
export type Content = { text: string } | { artifact: string };

export type Found =
  | { kind: 'step'; command: string; exit_code: number }
  | { kind: 'edit'; file: string }
  | { kind: 'decision'; chosen: string }
  | { kind: 'input'; by: string };

export interface Cited {
  stream: string;
  offset: number;
  /** What the log holds at the offset, when it is a record the story keeps. */
  found: Found | null;
}

export interface EditLine {
  offset: number;
  file: string;
  hunk: Content;
}

export interface DecisionLine {
  offset: number;
  attempt: number;
  by: string;
  so_far: string;
  question: string;
  options: string[];
  chosen: string;
  why: string;
  commit: string;
  evidence: Cited[];
  revert_check: { test: string; failed_without_fix: boolean; passed_with_fix: boolean } | null;
  proven: boolean;
  edits: EditLine[];
}

export interface Group {
  phase: Phase;
  decisions: DecisionLine[];
}

export interface PersonLine {
  offset: number;
  attempt: number | null;
  by: string;
  input: string;
  text: Content;
}

export type Rule =
  | 'claims_pass_without_run'
  | 'cited_step_failed'
  | 'evidence_not_recorded'
  | 'revert_check_not_proven'
  | 'fix_without_edit'
  | 'edits_never_run';

export interface Flag {
  rule: Rule;
  attempt: number;
  /** The decision the flag is about, or null for a whole attempt. */
  decision: number | null;
  offsets: number[];
  message: string;
}

export interface Story {
  item: string;
  attempts: { attempt: number; forked_from: [number, number] | null; done: boolean; closed: boolean }[];
  groups: Group[];
  people: PersonLine[];
  flags: Flag[];
}

export type Stories = ReadonlyMap<string, Story>;

export function stories(fold: Pick<Fold, 'storiesJson'>): Map<string, Story> {
  return new Map(Object.entries(JSON.parse(fold.storiesJson()) as Record<string, Story>));
}

export const PHASE_LABEL: Record<Phase, string> = {
  understand: 'Understand',
  reproduce: 'Reproduce',
  cause: 'Cause',
  fix: 'Fix',
  verify: 'Verify',
};

/** The text of a record's content, or null when it lives in an artifact the page has not read. */
export function contentText(c: Content): string | null {
  return 'text' in c ? c.text : null;
}

/** Flags about one decision, in the order the rules list them. */
export function flagsFor(story: Story, decision: number): Flag[] {
  return story.flags.filter((f) => f.decision === decision);
}

/** The flags about a whole attempt, not one decision. */
export function attemptFlags(story: Story): Flag[] {
  return story.flags.filter((f) => f.decision === null);
}

/** How many decisions the story holds, across attempts. */
export function decisionCount(story: Story): number {
  return story.groups.reduce((n, g) => n + g.decisions.length, 0);
}

/** What a cited offset shows, in words: `step 3: cargo test (exit 101)`, or `offset 9` when the log holds nothing the story keeps. */
export function citedText(c: Cited): string {
  const f = c.found;
  if (!f) return `offset ${c.offset}`;
  switch (f.kind) {
    case 'step': return `step ${c.offset}: ${f.command} (exit ${f.exit_code})`;
    case 'edit': return `edit ${c.offset}: ${f.file}`;
    case 'decision': return `decision ${c.offset}: ${f.chosen}`;
    case 'input': return `note ${c.offset} from ${f.by}`;
  }
}
