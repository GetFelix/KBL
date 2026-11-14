// The bar above the work: list or board, one select per recorded facet, what the
// filters hide in words, Clear filters, and Save view. Native selects, so every
// screen reader and keyboard already knows how to use them.
import { chordFor, displayChord } from '../keys.ts';
import { flatten, visible } from '../list.ts';
import type { ViewState } from '../state.ts';
import { activeCount, facets, summary, type Facet, type Layout } from '../views.ts';
import { h, kbd } from './dom.ts';

export interface FilterHandlers {
  layout(layout: Layout): void;
  filter(facet: Facet, value: string | null): void;
  clearFilters(): void;
  saveView(): void;
}

const ANY = '';

export function filterBar(on: FilterHandlers, mac: boolean) {
  const layoutButton = (layout: Layout, label: string) =>
    h('button', { type: 'button', class: 'segment', 'data-layout': layout, 'aria-pressed': 'false', onclick: () => on.layout(layout) }, label);
  const list = layoutButton('list', 'List');
  const board = layoutButton('board', 'Board');
  const selects = h('div', { class: 'filter-selects' });
  const text = h('span', {});
  const clear = h('button', { type: 'button', class: 'button ghost', onclick: on.clearFilters, hidden: true }, 'Clear filters');
  const save = h('button', { type: 'button', class: 'button ghost', onclick: on.saveView }, 'Save view');
  const el = h('div', { class: 'filter-bar' },
    h('div', { class: 'segmented', role: 'group', 'aria-label': 'Layout' }, list, board, kbd(displayChord(chordFor('layout'), mac))),
    h('div', { class: 'filters', role: 'group', 'aria-label': 'Filters' }, selects),
    // Polite, so a new filter is announced once it applies, without stealing focus.
    h('p', { class: 'filter-summary muted', 'aria-live': 'polite' }, text),
    clear,
    save,
  );
  let shown = '';
  return {
    el,
    /** The first filter, for the F key; null when nothing recorded a facet. */
    first: () => selects.querySelector('select'),
    render(s: ViewState) {
      for (const b of [list, board]) b.setAttribute('aria-pressed', String(b.dataset.layout === s.layout));
      const offered = facets(s.items);
      const key = JSON.stringify([offered, s.filters]);
      if (key !== shown) {
        shown = key;
        const hadFocus = (document.activeElement as HTMLElement | null)?.dataset.facet;
        selects.replaceChildren(
          ...offered.map(({ facet, label, values }) => {
            const id = `filter-${facet}`;
            const select = h('select', {
              id,
              'data-facet': facet,
              onchange: (e) => {
                const v = (e.target as HTMLSelectElement).value;
                on.filter(facet, v === ANY ? null : v);
              },
            },
              h('option', { value: ANY }, 'Any'),
              ...values.map(([v, n]) => h('option', { value: v }, `${v} (${n})`)),
            );
            // A filter from a link may name a value no item has now; keep it visible so it can be cleared.
            const current = s.filters[facet];
            if (current !== undefined && !values.some(([v]) => v === current)) select.append(h('option', { value: current }, `${current} (0)`));
            select.value = current ?? ANY;
            return h('label', { class: 'filter', for: id }, h('span', { class: 'filter-label' }, label), select);
          }),
        );
        if (hadFocus) selects.querySelector<HTMLElement>(`[data-facet="${hadFocus}"]`)?.focus();
      }
      const n = activeCount(s.filters);
      clear.hidden = n === 0;
      const shownCount = flatten(visible(s.items, s.query, s.filters)).length;
      const words = n ? `Showing ${shownCount} of ${s.items.length}: ${summary(s.filters)}` : '';
      if (text.textContent !== words) text.textContent = words;
    },
  };
}
