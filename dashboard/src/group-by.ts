// Group by: the fields an item row already has, as a table. A key adds one entry here and
// the list, the address, saved views, the toolbar and the palette all pick it up.
import type { ItemRow } from './fold.ts';
import { STAGES, stageLabel } from './stages.ts';

export interface GroupKey {
  id: string;
  /** Shown in the toolbar and the palette. */
  label: string;
  /** The item's value, or null when the log recorded none (those items group last, under `none`). */
  value(item: ItemRow): string | null;
  /** The group's heading for a value. */
  name(value: string): string;
  /** Orders two values; the default is natural text order. */
  compare?(a: string, b: string): number;
  /** The heading of the group of items with no value. */
  none: string;
}

const text = (a: string, b: string) => a.localeCompare(b, undefined, { numeric: true });
const some = (v: string | null | undefined) => (v === null || v === undefined || v === '' ? null : v);

export const GROUP_KEYS: readonly GroupKey[] = [
  {
    id: 'stage',
    label: 'Stage',
    value: (i) => some(i.stage),
    name: stageLabel,
    compare: (a, b) => {
      const at = (s: string) => {
        const k = STAGES.findIndex(([x]) => x === s);
        return k < 0 ? STAGES.length : k;
      };
      return at(a) - at(b) || text(a, b);
    },
    none: 'No stage',
  },
  { id: 'tier', label: 'Tier', value: (i) => some(i.tier === null || i.tier === undefined ? null : String(i.tier)), name: (v) => `Tier ${v}`, compare: text, none: 'No tier' },
  { id: 'repo', label: 'Repo', value: (i) => some(i.repo), name: (v) => v, none: 'No repo' },
  { id: 'kind', label: 'Kind', value: (i) => some(i.work), name: (v) => v.charAt(0).toUpperCase() + v.slice(1).replaceAll('_', ' '), none: 'No kind' },
  {
    id: 'assignee',
    label: 'Assignee',
    value: (i) => some(i.assignee && 'person' in i.assignee ? i.assignee.person : i.assignee && 'agent' in i.assignee ? `agent:${i.assignee.agent}` : null),
    // People first, then agents.
    compare: (a, b) => Number(a.startsWith('agent:')) - Number(b.startsWith('agent:')) || text(a, b),
    name: (v) => (v.startsWith('agent:') ? `Agent ${v.slice(6)}` : v),
    none: 'Unassigned',
  },
];

/** The "no grouping" choice, kept apart from the keys because it groups nothing. */
export const NO_GROUP = 'none';

export const isGroupBy = (v: string | null | undefined): v is string => v === NO_GROUP || GROUP_KEYS.some((k) => k.id === v);

/** What each view groups by until a person picks another. The inbox is cleared oldest first, so it stays one list. */
export function defaultGroup(view: string): string {
  return view === 'inbox' ? NO_GROUP : 'stage';
}

export const effectiveGroup = (view: string, group: string | undefined): string => (isGroupBy(group) ? group : defaultGroup(view));

export const groupLabel = (id: string): string => (id === NO_GROUP ? 'None' : (GROUP_KEYS.find((k) => k.id === id)?.label ?? id));

/** Names one collapsed group, per view and per grouping, so a collapse in one does not leak into another. */
export const collapseKey = (view: string, by: string, value: string): string => `${view}|${by}|${value}`;

/** Items split by `by`, keeping their order inside each group; groups ordered by the key, the empty one last. */
export function groupItems(items: readonly ItemRow[], by: string): { value: string; label: string; items: ItemRow[] }[] {
  const key = GROUP_KEYS.find((k) => k.id === by);
  if (!key) return [];
  const split = new Map<string, ItemRow[]>();
  for (const item of items) {
    const v = key.value(item) ?? '';
    const rows = split.get(v);
    if (rows) rows.push(item);
    else split.set(v, [item]);
  }
  const compare = key.compare ?? text;
  return [...split.entries()]
    .sort(([a], [b]) => (a === '' ? 1 : b === '' ? -1 : compare(a, b)))
    .map(([value, rows]) => ({ value, label: value === '' ? key.none : key.name(value), items: rows }));
}
