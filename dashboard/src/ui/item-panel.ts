// The side panel for the open item. It shows what the fold recorded, nothing inferred.
import { stageLabel } from '../list.ts';
import type { ViewState } from '../state.ts';
import { h } from './dom.ts';

const WHY = {
  missing: (id: string) => `The log has no item ${id}.`,
  stopped: () => 'The dashboard stopped before it finished reading the log.',
  reading: () => 'Still reading the log.',
};

export function itemPanel(onClose: () => void): { el: HTMLElement; render(s: ViewState): void; focus(): void } {
  // Focusable itself, so opening an item can move focus here without landing on Close.
  const el = h('aside', { class: 'panel', 'aria-labelledby': 'panel-title', tabindex: '-1', hidden: true });
  let shown = '';
  return {
    el,
    render(s) {
      const item = s.items.find((i) => i.id === s.open);
      const why = item ? '' : s.synced ? 'missing' : s.connection.kind === 'stopped' ? 'stopped' : 'reading';
      const key = s.open === null ? '' : `${s.open}|${item?.title}|${item?.stage}|${why}`;
      if (key === shown) return;
      shown = key;
      el.hidden = s.open === null;
      if (s.open === null) return el.replaceChildren();
      // A live record can redraw the panel under the focus; keep the focus in the panel.
      const hadFocus = el.contains(document.activeElement);
      const close = h('button', { type: 'button', class: 'button ghost', 'aria-label': 'Close the item', onclick: onClose }, 'Close');
      el.replaceChildren(
        h('header', { class: 'panel-head' }, h('span', { class: 'row-id' }, s.open), close),
        item
          ? h('div', {},
              h('h2', { id: 'panel-title', class: 'panel-title' }, item.title),
              h('dl', { class: 'fields' }, h('dt', {}, 'Stage'), h('dd', {}, stageLabel(item.stage))))
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
