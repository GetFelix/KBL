// The bar above the work: list or board, one select per recorded facet, what the
// filters hide in words, Clear filters, and Save view. Native selects, so every
// screen reader and keyboard already knows how to use them.
import { chordFor, displayChord } from '../keys.ts';
import { inScope, shownRows } from '../list.ts';
import type { ViewState } from '../state.ts';
import { activeCount, facets, summary, type Facet, type Layout } from '../views.ts';
import { h, kbd } from './dom.ts';

export interface FilterHandlers {
  layout(layout: Layout): void;
  children(): void;
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
  const children = h('button', { type: 'button', class: 'button ghost', 'aria-pressed': 'false', onclick: on.children }, 'Child cards', kbd(displayChord(chordFor('children'), mac)));
  const selects = h('div', { class: 'filter-selects' });
  const clear = h('button', { type: 'button', class: 'button ghost', onclick: on.clearFilters, hidden: true }, 'Clear filters');
  const save = h('button', { type: 'button', class: 'button ghost', onclick: on.saveView }, 'Save view');
  // Polite, so a new filter is announced once it applies, without stealing focus. The
  // text sits directly in the region so :empty can collapse it.
  const summaryEl = h('p', { class: 'filter-summary muted', 'aria-live': 'polite' });
  const el = h('div', { class: 'filter-bar' },
    h('div', { class: 'segmented', role: 'group', 'aria-label': 'Layout' }, list, board, kbd(displayChord(chordFor('layout'), mac))),
    children,
    h('div', { class: 'filters', role: 'group', 'aria-label': 'Filters' }, selects),
    summaryEl,
    clear,
    save,
  );
  // Selects are kept per facet and their options updated in place: rebuilding a
  // select under a keyboard or an open picker would close it and lose the place.
  const kept = new Map<Facet, { label: HTMLLabelElement; select: HTMLSelectElement }>();
  let last: ViewState | null = null;
  const control = (facet: Facet, text: string) => {
    let c = kept.get(facet);
    if (!c) {
      const id = `filter-${facet}`;
      const select = h('select', {
        id,
        'data-facet': facet,
        onchange: (e) => {
          const v = (e.target as HTMLSelectElement).value;
          on.filter(facet, v === ANY ? null : v);
        },
        // Updates that arrived while it had focus apply once it lets go.
        onblur: () => last && render(last),
      });
      c = { label: h('label', { class: 'filter', for: id }, h('span', { class: 'filter-label' }, text), select), select };
      kept.set(facet, c);
    }
    return c;
  };
  const setOptions = (select: HTMLSelectElement, options: [string, string][], value: string) => {
    options.forEach(([v, text], i) => {
      const o = select.options[i] ?? select.appendChild(h('option'));
      // Without a value attribute an option's value is its text, so "Any" would not match ''
      // and the select would show nothing. Set both, and the selection, explicitly.
      if (!o.hasAttribute('value') || o.value !== v) o.value = v;
      if (o.textContent !== text) o.textContent = text;
      if (o.selected !== (v === value)) o.selected = v === value;
    });
    while (select.options.length > options.length) select.options[select.options.length - 1].remove();
  };
  const render = (s: ViewState) => {
    last = s;
    for (const b of [list, board]) b.setAttribute('aria-pressed', String(b.dataset.layout === s.layout));
    children.setAttribute('aria-pressed', String(!!s.childCards));
    const scoped = inScope(s);
    const offered = facets(scoped, s.filters);
    const busy = document.activeElement;
    for (const [facet, c] of kept) {
      if (!offered.some((o) => o.facet === facet) && c.select !== busy) {
        c.label.remove();
        kept.delete(facet);
      }
    }
    offered.forEach(({ facet, label, values }, i) => {
      const c = control(facet, label);
      // Inserting only moves the new node, so a focused select is never detached.
      if (selects.children[i] !== c.label) selects.insertBefore(c.label, selects.children[i] ?? null);
      if (c.select === busy) return;
      const current = s.filters[facet];
      const options: [string, string][] = [[ANY, 'Any'], ...values.map(([v, n]): [string, string] => [v, `${v} (${n})`])];
      // A filter from a link may name a value no item has now; keep it visible so it can be cleared.
      if (current !== undefined && !values.some(([v]) => v === current)) options.push([current, `${current} (0)`]);
      setOptions(c.select, options, current ?? ANY);
    });
    const n = activeCount(s.filters);
    clear.hidden = n === 0;
    const shownCount = shownRows(s).length;
    const parts = [s.query.trim() && `search “${s.query.trim()}”`, summary(s.filters)].filter(Boolean);
    const words = parts.length ? `Showing ${shownCount} of ${scoped.length}: ${parts.join(', ')}` : '';
    if (summaryEl.textContent !== words) summaryEl.textContent = words;
  };
  return {
    el,
    /** The first filter, for the F key; null when nothing recorded a facet. */
    first: () => selects.querySelector('select'),
    /** Save view, where focus goes when the last saved view is deleted. */
    focusSave: () => save.focus(),
    render,
  };
}
