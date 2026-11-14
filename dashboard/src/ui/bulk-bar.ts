// Above the work: the bulk actions for the marked items, and the activity line that says
// what the last write did. The line is a status region that stays until the next write
// or Dismiss, so nobody has to read a refusal before a timer hides it (WCAG 2.2.1).
import type { ViewState } from '../state.ts';
import { CAN, summary } from '../writes.ts';
import { h } from './dom.ts';
import { tierSelect, type WriteHandlers } from './item-actions.ts';

export function bulkBar(on: WriteHandlers) {
  const count = h('span', { class: 'bulk-count' });
  const tier = tierSelect('bulk-tier', null);
  const setTier = h('button', { type: 'button', class: 'button small' }, 'Set tier');
  const accept = h('button', { type: 'button', class: 'button small' });
  const close = h('button', { type: 'button', class: 'button small danger' });
  const clear = h('button', { type: 'button', class: 'button ghost small', onclick: on.clearMarks }, 'Clear selection');
  const signIn = h('button', { type: 'button', class: 'button small', onclick: on.signIn }, 'Sign in to act');
  let ids = { tier: [] as string[], accept: [] as string[], close: [] as string[] };
  setTier.addEventListener('click', () => ids.tier.length && on.setTier(ids.tier, Number(tier.value)));
  accept.addEventListener('click', () => ids.accept.length && on.confirm('accept', ids.accept));
  close.addEventListener('click', () => ids.close.length && on.confirm('close', ids.close));
  const bar = h('section', { class: 'bulk', 'aria-label': 'Selected items', hidden: true },
    count,
    h('label', { for: 'bulk-tier', class: 'inline-label' }, 'Tier'), tier, setTier, accept, close, signIn, clear);

  const text = h('p', { class: 'activity-text', role: 'status' });
  const undo = h('button', { type: 'button', class: 'button small', onclick: on.undo }, 'Undo');
  const dismiss = h('button', { type: 'button', class: 'button ghost small', onclick: on.dismiss }, 'Dismiss');
  const activity = h('div', { class: 'activity' }, text, undo, dismiss);
  let dismissed: number | null = null;

  return {
    el: h('div', { class: 'writes' }, bar, activity),
    dismiss(batch: number | null) {
      dismissed = batch;
    },
    render(s: ViewState) {
      const marked = s.items.filter((i) => s.marked.includes(i.id));
      bar.hidden = marked.length === 0;
      const signedIn = s.tracker.kind === 'signed-in';
      ids = {
        tier: marked.filter(CAN.tier).map((i) => i.id),
        accept: marked.filter(CAN.accept).map((i) => i.id),
        close: marked.filter(CAN.close).map((i) => i.id),
      };
      count.textContent = `${marked.length} selected`;
      accept.textContent = `Accept ${ids.accept.length}…`;
      close.textContent = `Close ${ids.close.length}…`;
      // A button for an action none of the marked items allow would only be refused.
      accept.hidden = !signedIn || ids.accept.length === 0;
      close.hidden = !signedIn || ids.close.length === 0;
      for (const e of [tier, setTier, bar.querySelector('label')!]) (e as HTMLElement).hidden = !signedIn || ids.tier.length === 0;
      signIn.hidden = signedIn;

      const batch = s.writes.last === null || s.writes.last === dismissed ? [] : s.writes.list.filter((w) => w.batch === s.writes.last);
      const line = batch.length ? summary(batch) : null;
      // Kept in the page while idle: a live region added with its text is often not announced.
      activity.classList.toggle('idle', line === null);
      dismiss.hidden = line === null;
      const t = line?.text ?? '';
      if (text.textContent !== t) text.textContent = t;
      activity.dataset.tone = line?.tone ?? '';
      undo.hidden = !line?.undoable;
    },
  };
}
