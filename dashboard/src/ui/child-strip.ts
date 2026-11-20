// The strip under a root card or row: a chip with the sub-task count that opens the
// children as a list of links. A button beside the card, not inside it, because a
// card is a link and a link cannot hold a button.
import { chipText, type ChildSummary } from '../card.ts';
import { lead } from '../card.ts';
import { stageLabel } from '../list.ts';
import { h } from './dom.ts';

export interface StripHandlers {
  toggle(id: string): void;
  open(id: string): void;
}

/** What the strip shows, as a string, so a board or list redraws only when it changes. */
export function stripKey(c: ChildSummary | undefined, open: boolean): string {
  return c ? `${open}|${chipText(c)}|${c.kids.map((k) => `${k.id}:${k.stage}`).join(',')}` : '';
}

export function childStrip(root: string, c: ChildSummary, open: boolean, href: (id: string) => string, on: StripHandlers): HTMLElement {
  const listId = `children-${root}`;
  const chip = h('button', {
    type: 'button',
    class: 'chip',
    'data-chip': root,
    'aria-expanded': String(open),
    'aria-controls': listId,
    onclick: () => on.toggle(root),
  }, h('span', { class: 'chip-caret', 'aria-hidden': 'true' }, open ? '▾' : '▸'), chipText(c));
  const list = h('ul', { id: listId, class: 'child-list', hidden: !open },
    ...c.kids.map((k) => h('li', {}, h('a', {
      class: 'child-link',
      'data-child': k.id,
      href: href(k.id),
      onclick: (e) => {
        const m = e as MouseEvent;
        if (m.metaKey || m.ctrlKey || m.shiftKey || m.button !== 0) return;
        e.preventDefault();
        on.open(k.id);
      },
    }, `${lead(k)} ${k.title}`, h('span', { class: 'muted' }, ` ${stageLabel(k.stage)}`)))),
  );
  return h('div', { class: 'child-strip' }, chip, list);
}

/** The key of the chip or child link holding the focus, for `refocusChip` after a rebuild. */
export function focusedStrip(host: HTMLElement): string | undefined {
  const a = document.activeElement;
  if (!(a instanceof HTMLElement) || !host.contains(a)) return undefined;
  return a.dataset.chip ?? (a.dataset.child ? `child:${a.dataset.child}` : undefined);
}

/** Puts the focus back on a chip or child link that was rebuilt under it. */
export function refocusChip(id: string | undefined, host: ParentNode): void {
  if (id?.startsWith('child:')) host.querySelector<HTMLElement>(`[data-child="${CSS.escape(id.slice(6))}"]`)?.focus({ preventScroll: true });
  else if (id) host.querySelector<HTMLElement>(`[data-chip="${CSS.escape(id)}"]`)?.focus({ preventScroll: true });
}
