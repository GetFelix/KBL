// The side panel for the open item. It shows what the fold recorded, nothing inferred.
// Each entry in FIELDS adds one labelled row, so a new kind of detail is one module.
import { upstreamRef, upstreamUrl } from '../card.ts';
import type { ItemRow } from '../fold.ts';
import { rootStageLabel, triageChip, triageSkipReason } from '../list.ts';
import type { ViewState } from '../state.ts';
import { FACETS, valueOf } from '../views.ts';
import { byFocus, h } from './dom.ts';
import { itemHref } from './item-list.ts';
import { overlapField } from './overlap-section.ts';
import { storyField } from './story-section.ts';
import { handoffField } from './handoff-field.ts';
import { historySection } from './history-section.ts';
import { linkedSection } from './linked-section.ts';
import { nowSection } from './now-section.ts';
import { tickTimes, timelineSection } from './timeline-section.ts';
import { itemActions, type WriteHandlers } from './item-actions.ts';
import { assigneePerson, needsLine } from '../writes.ts';
import type { ActivityEvent } from '../activity.ts';
import { handoffKey } from '../handoff.ts';

export interface PanelHandlers extends WriteHandlers {
  closeItem(): void;
  open(id: string): void;
}

/**
 * One field of the open item. `key` is what the field's output depends on beyond the item's
 * title and stage, so the panel redraws when it changes. `render` returns a label and value, or null.
 */
export interface Field {
  key(item: ItemRow, s: ViewState): string;
  render(item: ItemRow, s: ViewState, on: PanelHandlers): [string, Node] | null;
}

const stageField: Field = {
  key: (item) => `${item.triage ?? ''}|${item.triage_skipped ?? ''}`,
  render: (item) => {
    const why = triageSkipReason(item);
    if (why) return ['Stage', document.createTextNode(`${rootStageLabel(item)} (${why})`)];
    const chip = triageChip(item);
    return ['Stage', document.createTextNode(chip ? `${rootStageLabel(item)} (${chip.toLowerCase()})` : rootStageLabel(item))];
  },
};

const needsField: Field = {
  key: (item) => `${item.needs ?? ''}|${assigneePerson(item) ?? ''}`,
  render: (item) => (item.needs ? ['Needs you', document.createTextNode(needsLine(item))] : null),
};

const assigneeField: Field = {
  key: (item) => assigneePerson(item) ?? '',
  render: (item) => {
    const who = assigneePerson(item);
    return who === null ? null : ['Assigned to', document.createTextNode(who)];
  },
};

// The upstream reference as a link when the source and id allow one; the KBL id is in the panel header.
const upstreamField: Field = {
  key: (item) => item.upstream ?? '',
  render: (item) => {
    const ref = upstreamRef(item);
    if (!ref) return null;
    const url = upstreamUrl(item);
    return ['Upstream', url ? h('a', { href: url, target: '_blank', rel: 'noopener noreferrer' }, ref) : document.createTextNode(ref)];
  },
};

// Only what the log recorded: a facet with no value is left out, not shown as "none".
const facetFields: Field[] = FACETS.map(([facet, label]) => ({
  key: (item) => valueOf(item, facet) ?? '',
  render: (item) => {
    const v = valueOf(item, facet);
    return v === null ? null : [label, document.createTextNode(v)];
  },
}));

// A new field is one module and one entry here.
const FIELDS: readonly Field[] = [upstreamField, stageField, needsField, assigneeField, ...facetFields, overlapField, handoffField, storyField];

const WHY = {
  missing: (id: string) => `The log has no item ${id}.`,
  stopped: () => 'The dashboard stopped before it finished reading the log.',
  reading: () => 'Still reading the log.',
  unreachable: () => 'The gateway cannot be reached, so the log has not been read. Trying again.',
};

export function itemPanel(on: PanelHandlers): { el: HTMLElement; render(s: ViewState): void; focus(): void } {
  // Focusable itself, so opening an item can move focus here without landing on Close.
  const el = h('aside', { class: 'panel', 'aria-labelledby': 'panel-title', tabindex: '-1', hidden: true });
  let shown = '';
  let failed = false;
  const actions = itemActions(on);
  const now = nowSection();
  const timeline = timelineSection();
  let latest: ViewState | null = null;
  // The clock moves between records: keep "40 s ago" true without a redraw.
  setInterval(() => {
    if (el.hidden) return;
    now.tick();
    tickTimes(el);
  }, 5000);
  const api = {
    el,
    render(s: ViewState) {
      latest = s;
      // A failed attempt stays the story until the socket is joined again.
      const c = s.connection;
      if (c.kind === 'retrying') failed = true;
      else if (c.kind !== 'loading' || c.linked) failed = false;
      const item = s.items.find((i) => i.id === s.open);
      const why = item ? '' : s.synced ? 'missing' : s.connection.kind === 'stopped' ? 'stopped' : failed ? 'unreachable' : 'reading';
      const events = (s.open && s.activity?.get(s.open)) || [];
      const key = s.open === null ? '' : `${s.open}|${item?.title}|${item?.stage}|${why}|${item ? FIELDS.map((f) => f.key(item, s)).join('|') : ''}|${item ? sectionsKey(item, s) : ''}`;
      // Links follow the view and query without a redraw, so a search keystroke does not rebuild the panel.
      for (const a of Array.from(el.querySelectorAll<HTMLAnchorElement>('a[data-item]'))) {
        const href = itemHref(a.dataset.item!, s);
        if (a.getAttribute('href') !== href) a.setAttribute('href', href);
      }
      // The actions update in place on every render; the rest only when its key changes.
      if (item) {
        actions.render(item, s);
        now.render(item, s, events);
      } else now.clear();
      if (key === shown) return;
      shown = key;
      el.hidden = s.open === null;
      if (s.open === null) return el.replaceChildren();
      // A live record can redraw the panel under the focus; keep the focus in the panel.
      const active = document.activeElement;
      const hadFocus = el.contains(active);
      const focusKey = active instanceof HTMLElement ? active.dataset.focus : undefined;
      const fallbackKey = active instanceof HTMLElement ? active.dataset.focusFallback : undefined;
      // A story record has no focus key; its offset finds it again after a redraw.
      const atKey = active instanceof HTMLElement ? active.dataset.at : undefined;
      const close = h('button', { type: 'button', class: 'button ghost', 'data-focus': 'close', 'aria-label': 'Close the item', onclick: on.closeItem }, 'Close');
      el.replaceChildren(
        h('header', { class: 'panel-head' }, h('span', { class: 'row-id' }, s.open), close),
        item
          ? h('div', {},
              h('h2', { id: 'panel-title', class: 'panel-title' }, item.title),
              now.el,
              actions.el,
              h('dl', { class: 'fields' },
                ...FIELDS.flatMap((f) => {
                  const got = f.render(item, s, on);
                  return got ? [h('dt', {}, got[0]), h('dd', {}, got[1])] : [];
                })),
              linkedSection(item, s, s.activity ?? new Map(), on),
              timeline.render(item.id, events, () => {
                shown = '';
                if (latest) api.render(latest);
              }),
              historySection(item, events, s.stories.get(item.id), () => el))
          : h('div', {},
              h('h2', { id: 'panel-title', class: 'panel-title' }, 'No such item'),
              h('p', { class: 'muted' }, WHY[why as keyof typeof WHY](s.open))),
      );
      if (hadFocus) {
        const same = focusKey ? byFocus(el, focusKey) : null;
        const near = fallbackKey ? byFocus(el, fallbackKey) : null;
        const record = atKey ? el.querySelector<HTMLElement>(`[data-at="${CSS.escape(atKey)}"]`) : null;
        (same ?? record ?? near ?? el).focus({ preventScroll: true });
      }
    },
    focus() {
      el.focus({ preventScroll: true });
      // Below 720 px the panel sits under the list, out of view.
      el.scrollIntoView({ block: 'nearest' });
    },
  };
  return api;
}

/** What the sections below the fields read, so the panel redraws when any of it moves. */
function sectionsKey(item: ItemRow, s: ViewState): string {
  const ev = s.activity?.get(item.id) ?? [];
  const kids = s.items.filter((i) => i.parent === item.id).map((i) => `${i.id}:${i.stage}:${s.activity?.get(i.id)?.length ?? 0}`);
  const blocking = s.blocked.filter((b) => b.id === item.id || b.waits_on.some((w) => w.id === item.id));
  return [ev.length, ev.at(-1)?.offset ?? '', item.pr ?? '', item.pr_phase ?? '', item.pr_checks ?? '', item.failing_check ?? '', kids.join(','), JSON.stringify(blocking), s.stories.get(item.id)?.rev ?? '', handoffKey(item, s.handoffs?.get(item.id))].join('|');
}
