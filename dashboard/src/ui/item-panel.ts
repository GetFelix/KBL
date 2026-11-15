// The side panel for the open item. It shows what the fold recorded, nothing inferred.
// Each entry in FIELDS adds one labelled row, so a new kind of detail is one module.
import type { ItemRow } from '../fold.ts';
import { stageLabel } from '../list.ts';
import type { ViewState } from '../state.ts';
import { FACETS, valueOf } from '../views.ts';
import { h } from './dom.ts';
import { itemHref } from './item-list.ts';
import { overlapField } from './overlap-section.ts';
import { waitsOnField } from './waits-on-field.ts';
import { itemActions, type WriteHandlers } from './item-actions.ts';
import { needsText } from '../writes.ts';

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
  key: () => '',
  render: (item) => ['Stage', document.createTextNode(stageLabel(item.stage))],
};

const needsField: Field = {
  key: (item) => item.needs ?? '',
  render: (item) => (item.needs ? ['Needs you', document.createTextNode(needsText(item.needs))] : null),
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
const FIELDS: readonly Field[] = [stageField, needsField, ...facetFields, waitsOnField, overlapField];

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
  return {
    el,
    render(s) {
      // A failed attempt stays the story until the socket is joined again.
      const c = s.connection;
      if (c.kind === 'retrying') failed = true;
      else if (c.kind !== 'loading' || c.linked) failed = false;
      const item = s.items.find((i) => i.id === s.open);
      const why = item ? '' : s.synced ? 'missing' : s.connection.kind === 'stopped' ? 'stopped' : failed ? 'unreachable' : 'reading';
      const key = s.open === null ? '' : `${s.open}|${item?.title}|${item?.stage}|${why}|${item ? FIELDS.map((f) => f.key(item, s)).join('|') : ''}`;
      // Links follow the view and query without a redraw, so a search keystroke does not rebuild the panel.
      for (const a of Array.from(el.querySelectorAll<HTMLAnchorElement>('a[data-item]'))) {
        const href = itemHref(a.dataset.item!, s);
        if (a.getAttribute('href') !== href) a.setAttribute('href', href);
      }
      // The actions update in place on every render; the rest only when its key changes.
      if (item) actions.render(item, s);
      if (key === shown) return;
      shown = key;
      el.hidden = s.open === null;
      if (s.open === null) return el.replaceChildren();
      // A live record can redraw the panel under the focus; keep the focus in the panel.
      const active = document.activeElement;
      const hadFocus = el.contains(active);
      const focusKey = active instanceof HTMLElement ? active.dataset.focus : undefined;
      const close = h('button', { type: 'button', class: 'button ghost', 'data-focus': 'close', 'aria-label': 'Close the item', onclick: on.closeItem }, 'Close');
      el.replaceChildren(
        h('header', { class: 'panel-head' }, h('span', { class: 'row-id' }, s.open), close),
        item
          ? h('div', {},
              h('h2', { id: 'panel-title', class: 'panel-title' }, item.title),
              h('dl', { class: 'fields' },
                ...FIELDS.flatMap((f) => {
                  const got = f.render(item, s, on);
                  return got ? [h('dt', {}, got[0]), h('dd', {}, got[1])] : [];
                })),
              actions.el)
          : h('div', {},
              h('h2', { id: 'panel-title', class: 'panel-title' }, 'No such item'),
              h('p', { class: 'muted' }, WHY[why as keyof typeof WHY](s.open))),
      );
      if (hadFocus) {
        const same = focusKey ? el.querySelector<HTMLElement>(`[data-focus="${focusKey}"]`) : null;
        (same ?? el).focus({ preventScroll: true });
      }
    },
    focus() {
      el.focus({ preventScroll: true });
      // Below 720 px the panel sits under the list, out of view.
      el.scrollIntoView({ block: 'nearest' });
    },
  };
}
