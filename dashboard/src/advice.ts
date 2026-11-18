// A person's adjustment of triage's advice on a proposal: what it waits on and how it should
// be split. Pure: the inbox row draws the form and main.ts sends the write.
import type { ItemRow } from './fold.ts';

/** The log format from which the tracker takes `depends_on` and `split` on a proposal edit. */
export const ADVICE_FORMAT = 17;
/** The most dependencies and split titles the tracker takes, and the longest title in bytes. */
export const MAX_ADVICE_LIST = 8;
export const MAX_SPLIT_TITLE = 256;

/** Whether the item holds triage's answer that a person may adjust: unaccepted, answered, and a log that takes the edit. */
export const canAdvise = (i: ItemRow): boolean => (i.format ?? 0) >= ADVICE_FORMAT && i.stage === 'triage' && i.accepted !== true && i.proposal?.at != null;

/** The ids in a field: separated by commas or spaces, each once. */
export const parseDeps = (text: string): string[] => [...new Set(text.split(/[\s,]+/).filter(Boolean))];

/** One split title per line. */
export const parseSplit = (text: string): string[] => text.split('\n').map((l) => l.trim()).filter(Boolean);

export interface Advice {
  depends_on: string[];
  split: string[];
}

export const adviceText = (p: Advice): { deps: string; split: string } => ({ deps: p.depends_on.join(', '), split: p.split.join('\n') });

/** Whether the fields differ from what triage said, so a save that changes nothing is not sent. */
export const adviceChanged = (p: Advice, deps: readonly string[], split: readonly string[]): boolean =>
  deps.join('\u0000') !== p.depends_on.join('\u0000') || split.join('\u0000') !== p.split.join('\u0000');

/** What is wrong with the advice, in the tracker's terms, or null. The tracker and the fold still decide. */
export function checkAdvice(item: ItemRow, deps: readonly string[], split: readonly string[], items: readonly ItemRow[]): string | null {
  if (deps.length > MAX_ADVICE_LIST || split.length > MAX_ADVICE_LIST) return `At most ${MAX_ADVICE_LIST} dependencies and ${MAX_ADVICE_LIST} split titles.`;
  const known = new Set(items.map((i) => i.id));
  const own = new Set(items.filter((i) => i.parent === item.id).map((i) => i.id));
  for (const d of deps) {
    if (d === item.id) return `${d} cannot wait on itself.`;
    if (own.has(d)) return `${d} is this item’s own triage work, not another item.`;
    if (!known.has(d)) return `${d} is not an item in the log.`;
  }
  const long = split.find((t) => new TextEncoder().encode(t).length > MAX_SPLIT_TITLE);
  return long ? `A split title is over ${MAX_SPLIT_TITLE} bytes: “${long.slice(0, 40)}…”.` : null;
}
