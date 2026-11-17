// Trust in agents' summaries, from the tracker's `summary-trust` read. A team figure: the
// answer names no agent, model or person, and the page adds none.

export interface Trust {
  days: number;
  /** Marks written in the window, by the standing mark of each attempt. */
  rated: number;
  accurate: number;
  notAccurate: number;
  /** Thousandths accurate; null with nothing rated. */
  permille: number | null;
  /** Finished attempts with no mark, as of now. */
  unrated: number;
  /** Marks below this many are too few to read a share from. */
  minSample: number;
  /** Marks the tracker could not place in time, so they are not counted. */
  undated: number;
}

const count = (v: unknown): number | null => (typeof v === 'number' && Number.isSafeInteger(v) && v >= 0 ? v : null);

/** The tracker's answer as a `Trust`, or null when it is not one. */
export function parseTrust(body: unknown): Trust | null {
  const b = body as { days?: unknown; stats?: Record<string, unknown> } | null;
  const s = b?.stats;
  if (!s || typeof s !== 'object') return null;
  const days = count(b?.days);
  const rated = count(s.rated);
  const accurate = count(s.accurate);
  const notAccurate = count(s.not_accurate);
  const unrated = count(s.unrated);
  const minSample = count(s.min_sample);
  if (days === null || rated === null || accurate === null || notAccurate === null || unrated === null || minSample === null) return null;
  const permille = s.accurate_permille === null ? null : count(s.accurate_permille);
  if (s.accurate_permille !== null && permille === null) return null;
  if (s.undated !== undefined && count(s.undated) === null) return null;
  return { days, rated, accurate, notAccurate, permille, unrated, minSample, undated: count(s.undated) ?? 0 };
}

export interface TrustText {
  /** The counts, which are shown first so a small sample is never hidden behind a percentage. */
  headline: string;
  /** The share, or null when there is nothing to take a share of. */
  share: string | null;
  /** True below the minimum sample: the share is shown muted with the warning. */
  small: boolean;
  warning: string | null;
  unrated: string;
  undated: string | null;
}

const plural = (n: number, one: string, many: string) => `${n.toLocaleString('en-US')} ${n === 1 ? one : many}`;

export function trustText(t: Trust): TrustText {
  const window = t.days === 1 ? 'the last day' : `the last ${t.days} days`;
  const small = t.rated < t.minSample;
  return {
    headline: t.rated === 0 ? `No summaries marked in ${window}` : `${t.accurate.toLocaleString('en-US')} of ${plural(t.rated, 'summary', 'summaries')} marked accurate in ${window}`,
    share: t.permille === null ? null : `${Math.floor(t.permille / 10)}% accurate`,
    small,
    warning: t.rated === 0 ? null : small ? `Only ${plural(t.rated, 'mark', 'marks')}: too few to read a trend from (fewer than ${t.minSample}).` : null,
    unrated: `${t.unrated.toLocaleString('en-US')} finished ${t.unrated === 1 ? 'attempt has' : 'attempts have'} no mark yet.`,
    undated: t.undated > 0 ? `${plural(t.undated, 'mark', 'marks')} could not be placed in time and ${t.undated === 1 ? 'is' : 'are'} left out.` : null,
  };
}
