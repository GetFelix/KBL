// The Spend dialog: what claims cost, per root, reviewer tier, repo and model. It shows the
// fold's derived costs and the usage behind them, so the total is a lower bound and the dialog says so.
import { dollars, priceText, stalePrices, tables, type ModelRow, type PriceUnit, type Spend, type SpendRow } from '../spend.ts';
import type { ItemRow } from '../fold.ts';
import type { ViewState } from '../state.ts';
import { modal } from './dialog.ts';
import { h } from './dom.ts';
import { itemHref } from './item-list.ts';

export interface SpendHandlers {
  /** Opens an item in the panel, after the dialog closed. */
  open(id: string): void;
  /** Whether the viewer is signed in to the tracker, which is what setting a price needs. */
  canSetPrice(): boolean;
  /** Sends a price typed in dollars; resolves to why it was refused, or null once written. */
  setPrice(model: string, unit: PriceUnit, dollarsText: string): Promise<string | null>;
  /** The time now, and the age in days after which a price shows "check pricing". */
  now(): number;
  priceAgeDays: number;
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

const UNITS: readonly [PriceUnit, string][] = [['input', 'Input tokens'], ['output', 'Output tokens'], ['cache_read', 'Cache read tokens'], ['cache_write', 'Cache write tokens'], ['aic', 'AI credits']];

/** Models whose usage waits for a price: not a Needs-you item, and nothing is blocked. A person who is signed in can price one here. */
function waiting(list: ModelRow[], on: SpendHandlers, status: Node): Node | null {
  const names = list.filter((r) => r.unpriced).map((r) => r.model);
  if (!names.length) return null;
  const can = on.canSetPrice();
  return h('section', { class: 'price-card', 'aria-labelledby': 'spend-waiting' },
    h('h3', { id: 'spend-waiting' }, `Waiting for a price (${names.length})`),
    h('p', { class: 'muted small' }, 'Work goes on. Until a price is set these models are charged the cost floor only; a price set now prices the usage that waited.'),
    h('ul', { class: 'price-list' }, ...names.map((model) => {
      const unit = h('select', { 'aria-label': `Unit for ${model}` }, ...UNITS.map(([v, label]) => h('option', { value: v }, label))) as HTMLSelectElement;
      const price = h('input', { type: 'text', inputmode: 'decimal', size: '7', 'aria-label': `Dollars per million tokens for ${model}`, placeholder: '$ per million' }) as HTMLInputElement;
      const set = h('button', {
        type: 'button',
        class: 'button',
        onclick: async () => {
          set.disabled = true;
          const why = await on.setPrice(model, unit.value as PriceUnit, price.value);
          set.disabled = false;
          status.textContent = why ?? `Price set for ${model}.`;
        },
      }, 'Set price');
      return h('li', { 'data-model': model }, h('span', { class: 'price-model' }, model),
        can ? h('span', { class: 'price-form' }, unit, price, set) : h('span', { class: 'muted small' }, 'Sign in to the tracker to set a price.'));
    })),
    status,
  );
}

function models(list: ModelRow[], stale: Map<string, number>, s: Spend): Node {
  return h('section', {},
    h('h3', { id: 'spend-models' }, 'By model'),
    list.length === 0
      ? h('p', { class: 'muted' }, 'No claim has reported usage yet.')
      : h('table', { class: 'spend-table models', 'aria-labelledby': 'spend-models' },
          h('thead', {}, h('tr', {}, ...['Model', 'Unit', 'Usage'].map((c) => h('th', { scope: 'col' }, c)), h('th', { scope: 'col', class: 'num' }, 'Cost'))),
          h('tbody', {}, ...list.map((r) =>
            h('tr', { 'data-key': r.model }, h('th', { scope: 'row' }, r.model, stale.has(r.model) ? h('span', { class: 'muted small stale-price', title: `Priced ${stale.get(r.model)} days ago: ${priceText(s, r.model)}` }, ' check pricing') : null), h('td', {}, r.unit), h('td', { class: 'usage' }, r.usage || 'none'), h('td', { class: 'num' }, modelCost(r))))),
        ),
  );
}

export function spendView(on: SpendHandlers) {
  const m = modal('Spend', 'spend');
  let drawn = '';
  const close = h('button', { type: 'button', class: 'button ghost', onclick: () => m.close() }, 'Close');
  return {
    el: m.el,
    isOpen: m.isOpen,
    /** Draws the tables, or a loading note when `spend` is null, and opens the dialog; if it is already open, a live record redraws it in place. */
    open(spend: Spend | null, items: readonly ItemRow[], s: ViewState) {
      const wasOpen = m.isOpen();
      // A live record that changes nothing shown must not rebuild the dialog and drop the keyboard focus.
      const key = JSON.stringify([spend, items.map((i) => [i.id, i.title])]);
      if (wasOpen && spend !== null && key === drawn) return;
      drawn = spend === null ? '' : key;
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
        h('p', { class: 'muted small spend-note' }, 'A lower bound: lapsed claims and unpriced models are charged only the cost floor (a minimum charge per claim), and reported usage is the agent\'s own word.'),
        section('Per root', 'spend-roots', 'Root', t.roots, 'No claim has recorded a cost yet.', root),
        section('Per reviewer tier (reviews only)', 'spend-tiers', 'Tier', t.tiers, 'No review has recorded a cost yet.', text, false),
        section('Per repo', 'spend-repos', 'Repo', t.repos, 'No repo has recorded a cost yet.', text),
        ...[waiting(t.models, on, h('p', { class: 'muted small', role: 'status' }))].filter((n) => n !== null),
        models(t.models, stalePrices(spend, on.now(), on.priceAgeDays), spend),
        h('p', { class: 'muted small spend-note' }, 'The total can be more than the By model costs: each claim is charged at least a minimum, the cost floor, and models without a price are charged only that.'),
      );
      if (wasOpen) return;
      m.open();
      close.focus();
    },
  };
}
