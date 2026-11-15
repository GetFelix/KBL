// The Spend dialog: what claims cost, per root, reviewer tier and repo. It shows the
// fold's recorded costs, so the total is a lower bound and the dialog says so.
import { dollars, tables, type Spend, type SpendRow } from '../spend.ts';
import type { ItemRow } from '../fold.ts';
import type { ViewState } from '../state.ts';
import { modal } from './dialog.ts';
import { h } from './dom.ts';
import { itemHref } from './item-list.ts';

export interface SpendHandlers {
  /** Opens an item in the panel, after the dialog closed. */
  open(id: string): void;
}

function section(title: string, id: string, first: string, list: SpendRow[], empty: string, cell: (r: SpendRow) => Node): Node {
  return h('section', {},
    h('h3', { id }, title),
    list.length === 0
      ? h('p', { class: 'muted' }, empty)
      : h('table', { class: 'spend-table', 'aria-labelledby': id },
          h('thead', {}, h('tr', {}, h('th', { scope: 'col' }, first), h('th', { scope: 'col', class: 'num' }, 'Spent'), h('th', { scope: 'col', class: 'num' }, 'Share'))),
          h('tbody', {}, ...list.map((r) =>
            h('tr', { 'data-key': r.key }, h('th', { scope: 'row' }, cell(r)), h('td', { class: 'num' }, dollars(r.micros)), h('td', { class: 'num muted' }, `${r.percent}%`)))),
        ),
  );
}

export function spendView(on: SpendHandlers) {
  const m = modal('Spend', 'spend');
  const close = h('button', { type: 'button', class: 'button ghost', onclick: () => m.close() }, 'Close');
  return {
    el: m.el,
    isOpen: m.isOpen,
    open(spend: Spend, items: readonly ItemRow[], s: ViewState) {
      const t = tables(spend, items);
      const named = (r: SpendRow) => [r.label, r.hint ? h('span', { class: 'muted' }, ` ${r.hint}`) : null];
      const text = (r: SpendRow) => h('span', {}, ...named(r));
      const root = (r: SpendRow) =>
        h('a', {
          href: itemHref(r.key, s),
          'data-item': r.key,
          onclick: (e) => {
            const k = e as MouseEvent;
            if (k.metaKey || k.ctrlKey || k.shiftKey || k.button !== 0) return;
            e.preventDefault();
            m.close();
            on.open(r.key);
          },
        }, ...named(r));
      m.body.replaceChildren(
        h('header', { class: 'dialog-head' }, h('h2', {}, 'Spend'), close),
        h('p', { class: 'spend-total' }, h('strong', {}, dollars(t.total)), ' spent on claims'),
        h('p', { class: 'muted small spend-note' }, 'A lower bound: a claim that lapsed recorded no cost, so it is not counted here.'),
        section('Per root', 'spend-roots', 'Root', t.roots, 'No claim has recorded a cost yet.', root),
        section('Per reviewer tier', 'spend-tiers', 'Tier', t.tiers, 'No review has recorded a cost yet.', text),
        section('Per repo', 'spend-repos', 'Repo', t.repos, 'No repo has recorded a cost yet.', text),
      );
      m.open();
      close.focus();
    },
  };
}
