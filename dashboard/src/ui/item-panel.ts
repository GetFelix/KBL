// The side panel for the open item. It shows what the fold recorded, nothing inferred.
import { stageLabel } from '../list.ts';
import type { ViewState } from '../state.ts';
import { h } from './dom.ts';

export function itemPanel(onClose: () => void): { el: HTMLElement; render(s: ViewState): void } {
  const el = h('aside', { class: 'panel', 'aria-labelledby': 'panel-title', hidden: true });
  let shown = '';
  return {
    el,
    render(s) {
      const item = s.items.find((i) => i.id === s.open);
      const key = s.open === null ? '' : `${s.open}|${item?.title}|${item?.stage}`;
      if (key === shown) return;
      shown = key;
      el.hidden = s.open === null;
      if (s.open === null) return el.replaceChildren();
      const close = h('button', { type: 'button', class: 'button ghost', 'aria-label': 'Close the item', onclick: onClose }, 'Close');
      el.replaceChildren(
        h('header', { class: 'panel-head' }, h('span', { class: 'row-id' }, s.open), close),
        item
          ? h('div', {},
              h('h2', { id: 'panel-title', class: 'panel-title' }, item.title),
              h('dl', { class: 'fields' }, h('dt', {}, 'Stage'), h('dd', {}, stageLabel(item.stage))))
          : h('div', {},
              h('h2', { id: 'panel-title', class: 'panel-title' }, 'No such item'),
              h('p', { class: 'muted' }, s.connection.kind === 'live' ? `The log has no item ${s.open}. The link may be from another tracker.` : 'Still reading the log.')),
      );
    },
  };
}
