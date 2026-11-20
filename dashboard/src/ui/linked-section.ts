// The panel's Linked work: the pull request with its checks, child work with the answer each
// recorded, and what this item waits on or holds back. Each id opens that item.
import { linked, type Activities } from '../activity.ts';
import { blockerLines, blockerNote } from '../fold.ts';
import { parsePr, prText } from '../card.ts';
import type { ItemRow } from '../fold.ts';
import { childLink } from './child-strip.ts';
import type { ViewState } from '../state.ts';
import { h } from './dom.ts';
import { itemHref } from './item-list.ts';
import type { PanelHandlers } from './item-panel.ts';

function itemLink(id: string, s: ViewState, on: PanelHandlers, focus = 'linked'): HTMLElement {
  return h('a', {
    class: 'blocker-id',
    'data-item': id,
    'data-focus': `${focus}:${id}`,
    href: itemHref(id, s),
    onclick: (e) => {
      const m = e as MouseEvent;
      if (m.metaKey || m.ctrlKey || m.shiftKey || m.button !== 0) return;
      e.preventDefault();
      on.open(id);
    },
  }, id);
}

/** The pull request as a link when its id names a GitHub repository. */
export function prLink(item: ItemRow): Node | null {
  const t = prText(item);
  if (!t) return null;
  const text = item.failing_check ? `${t}: ${item.failing_check}` : t;
  const url = item.pr ? parsePr(item.pr).url : null;
  return url && (item.source === 'github' || /^https?:/.test(item.pr ?? ''))
    ? h('a', { 'data-focus': 'pr-link', href: url, target: '_blank', rel: 'noopener noreferrer' }, text)
    : document.createTextNode(text);
}

/** Null when there is nothing linked to show. */
export function linkedSection(item: ItemRow, s: ViewState, acts: Activities, on: PanelHandlers): HTMLElement | null {
  const l = linked(item, s.items, s.blocked, acts);
  const pr = prLink(item);
  const row = s.blocked.find((b) => b.id === item.id);
  const waits = row ? blockerLines(row) : [];
  const related = item.related ?? [];
  if (!pr && l.children.length + waits.length + l.blocks.length + related.length === 0) return null;
  const group = (title: string, ...lis: HTMLElement[]) => (lis.length ? [h('h4', {}, title), h('ul', { class: title === 'Waits on' ? 'blockers' : 'linked' }, ...lis)] : []);
  return h('section', { class: 'linked-work', 'aria-labelledby': 'linked-title' },
    h('h3', { id: 'linked-title', class: 'section-title' }, 'Linked work'),
    ...group('Pull request', ...(pr ? [h('li', {}, pr)] : [])),
    ...group('Child work', ...l.children.map((c) => h('li', { class: 'child-item' },
      childLink(s.items.find((i) => i.id === c.id) ?? { id: c.id, title: c.title, stage: c.stage, work: c.work } as ItemRow, { 'data-item': c.id, 'data-focus': `linked:${c.id}` }, itemHref(c.id, s), () => on.open(c.id)),
      c.answer ? h('p', { class: 'linked-answer' }, h('span', { class: 'story-label' }, 'Answer '), c.answer) : null))),
    // The same list the Blocked view shows, with where each blocker lives.
    h('div', { class: 'blockers-box' }, ...group('Waits on', ...waits.map((b) => h('li', {}, itemLink(b.id, s, on, 'blocker'), b.title ? h('span', {}, ` ${b.title}`) : null, blockerNote(b) ? h('span', { class: 'muted' }, ` ${blockerNote(b)}`) : null)))),
    // Not a hold: a related item is neither waited on nor blocked by this one.
    ...group('Related', ...related.map((id) => h('li', {}, itemLink(id, s, on), ` ${s.items.find((x) => x.id === id)?.title ?? ''}`))),
    ...group('Blocks', ...l.blocks.map((b) => h('li', {}, itemLink(b.id, s, on), ` ${b.title}`))));
}
