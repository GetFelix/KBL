// The side panel for the open item. It shows what the fold recorded, nothing inferred.
// Each entry in FIELDS adds one labelled row, so a new kind of detail is one function.
import { blockerLines, blockerNote, type ItemRow } from '../fold.ts';
import { stageLabel } from '../list.ts';
import type { ViewState } from '../state.ts';
import { h } from './dom.ts';
import { itemHref } from './item-list.ts';

export interface PanelHandlers {
  closeItem(): void;
  open(id: string): void;
}

/** One field of the open item: its label and value, or null when it has nothing to show. */
type Field = (item: ItemRow, s: ViewState, on: PanelHandlers) => [string, Node] | null;

const FIELDS: readonly Field[] = [
  (item) => ['Stage', document.createTextNode(stageLabel(item.stage))],
  (item, s, on) => {
    const row = s.blocked.find((b) => b.id === item.id);
    if (!row) return null;
    // Each blocker is a link to it, so a person can walk the chain from the panel.
    const lines = blockerLines(row).map((b) =>
      h('li', {},
        h('a', {
          class: 'blocker-id',
          href: itemHref(b.id, s),
          onclick: (e) => {
            const m = e as MouseEvent;
            if (m.metaKey || m.ctrlKey || m.shiftKey || m.button !== 0) return;
            e.preventDefault();
            on.open(b.id);
          },
        }, b.id),
        b.title ? h('span', {}, ` ${b.title}`) : null,
        blockerNote(b) ? h('span', { class: 'muted' }, ` ${blockerNote(b)}`) : null,
      ),
    );
    return ['Waits on', h('ul', { class: 'blockers' }, ...lines)];
  },
];

/** What the panel's content depends on beyond the item row, so it redraws when that changes. */
function extra(item: ItemRow | undefined, s: ViewState): string {
  return item ? JSON.stringify(s.blocked.find((b) => b.id === item.id) ?? null) : '';
}

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
  return {
    el,
    render(s) {
      // A failed attempt stays the story until the socket is joined again.
      const c = s.connection;
      if (c.kind === 'retrying') failed = true;
      else if (c.kind !== 'loading' || c.linked) failed = false;
      const item = s.items.find((i) => i.id === s.open);
      const why = item ? '' : s.synced ? 'missing' : s.connection.kind === 'stopped' ? 'stopped' : failed ? 'unreachable' : 'reading';
      const key = s.open === null ? '' : `${s.open}|${item?.title}|${item?.stage}|${why}|${extra(item, s)}|${s.view}|${s.query}`;
      if (key === shown) return;
      shown = key;
      el.hidden = s.open === null;
      if (s.open === null) return el.replaceChildren();
      // A live record can redraw the panel under the focus; keep the focus in the panel.
      const hadFocus = el.contains(document.activeElement);
      const close = h('button', { type: 'button', class: 'button ghost', 'aria-label': 'Close the item', onclick: on.closeItem }, 'Close');
      el.replaceChildren(
        h('header', { class: 'panel-head' }, h('span', { class: 'row-id' }, s.open), close),
        item
          ? h('div', {},
              h('h2', { id: 'panel-title', class: 'panel-title' }, item.title),
              h('dl', { class: 'fields' },
                ...FIELDS.flatMap((f) => {
                  const got = f(item, s, on);
                  return got ? [h('dt', {}, got[0]), h('dd', {}, got[1])] : [];
                })))
          : h('div', {},
              h('h2', { id: 'panel-title', class: 'panel-title' }, 'No such item'),
              h('p', { class: 'muted' }, WHY[why as keyof typeof WHY](s.open))),
      );
      if (hadFocus) el.focus({ preventScroll: true });
    },
    focus() {
      el.focus({ preventScroll: true });
      // Below 720 px the panel sits under the list, out of view.
      el.scrollIntoView({ block: 'nearest' });
    },
  };
}
