// The Spend dialog: what claims cost, per root, reviewer tier, repo and model. It shows the
// fold's derived costs and the usage behind them, so the total is a lower bound and the dialog says so.
import { dollars, tables, type ModelRow, type Spend, type SpendRow } from '../spend.ts';
import type { ItemRow } from '../fold.ts';
import type { ViewState } from '../state.ts';
import { modal } from './dialog.ts';
import { h } from './dom.ts';
import { itemHref } from './item-list.ts';

export interface SpendHandlers {
  /** Opens an item in the panel, after the dialog closed. */
  open(id: string): void;
}

function section(title: string, id: string, first: string, list: SpendRow[], empty: string, cell: (r: SpendRow) => Node, share = true): Node {
  return h('section', {},
    h('h3', { id }, title),
    list.length === 0
      ? h('p', { class: 'muted' }, empty)
      : h('table', { class: 'spend-table', 'aria-labelledby': id },
          h('thead', {}, h('tr', {}, h('th', { scope: 'col' }, first), h('th', { scope: 'col', class: 'num' }, 'Spent'), share ? h('th', { scope: 'col', class: 'num' }, 'Share') : null)),
          h('tbody', {}, ...list.map((r) =>
            h('tr', { 'data-key': r.key }, h('th', { scope: 'row' }, cell(r)), h('td', { class: 'num' }, dollars(r.micros)), share ? h('td', { class: 'num muted' }, `${r.percent}%`) : null))),
        ),
  );
}

/** A model's cost, or "unpriced" when it has none to show. A partly priced model shows both. */
function modelCost(r: ModelRow): Node {
  if (!r.unpriced) return document.createTextNode(dollars(r.micros));
  const tag = h('span', { class: 'unpriced' }, 'unpriced');
  return r.micros > 0 ? h('span', {}, `${dollars(r.micros)} + `, tag) : tag;
}

function models(list: ModelRow[]): Node {
  return h('section', {},
    h('h3', { id: 'spend-models' }, 'By model'),
    list.length === 0
      ? h('p', { class: 'muted' }, 'No claim has reported usage yet.')
      : h('table', { class: 'spend-table models', 'aria-labelledby': 'spend-models' },
          h('thead', {}, h('tr', {}, ...['Model', 'Unit', 'Usage'].map((c) => h('th', { scope: 'col' }, c)), h('th', { scope: 'col', class: 'num' }, 'Cost'))),
          h('tbody', {}, ...list.map((r) =>
            h('tr', { 'data-key': r.model }, h('th', { scope: 'row' }, r.model), h('td', {}, r.unit), h('td', { class: 'usage' }, r.usage || 'none'), h('td', { class: 'num' }, modelCost(r))))),
        ),
  );
}

export function spendView(on: SpendHandlers) {
  const m = modal('Spend', 'spend');
  const close = h('button', { type: 'button', class: 'button ghost', onclick: () => m.close() }, 'Close');
  return {
    el: m.el,
    isOpen: m.isOpen,
    /** Draws the tables, or a loading note when `spend` is null, and opens the dialog; if it is already open, a live record redraws it in place. */
    open(spend: Spend | null, items: readonly ItemRow[], s: ViewState) {
      const wasOpen = m.isOpen();
      if (spend === null) {
        m.body.replaceChildren(
          h('header', { class: 'dialog-head' }, h('h2', {}, 'Spend'), close),
          h('p', { class: 'muted', role: 'status' }, 'Still reading the history. Spend appears when it is loaded.'),
        );
        if (wasOpen) return;
        m.open();
        return close.focus();
      }
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
        h('p', { class: 'muted small spend-note' }, 'A lower bound: lapsed claims and unpriced models are charged only the cost floor, and reported usage is the agent\'s own word.'),
        section('Per root', 'spend-roots', 'Root', t.roots, 'No claim has recorded a cost yet.', root),
        section('Per reviewer tier (reviews only)', 'spend-tiers', 'Tier', t.tiers, 'No review has recorded a cost yet.', text, false),
        section('Per repo', 'spend-repos', 'Repo', t.repos, 'No repo has recorded a cost yet.', text),
        models(t.models),
      );
      if (wasOpen) return;
      m.open();
      close.focus();
    },
  };
}
