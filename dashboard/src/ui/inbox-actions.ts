// The strip under each Needs-you row: answer a question, accept a proposal or decide what
// to do with a stuck item, without opening the panel. Built once per item and what it
// offers, then updated in place, so a live record never wipes a half-typed answer.
// The writes are the panel's own: the same routes, the same token, the same refusals.
import type { ItemRow } from '../fold.ts';
import { inboxKind } from '../inbox.ts';
import type { ViewState } from '../state.ts';
import { CAN, checkAnswer, pending, shownTier, UNCLEAR } from '../writes.ts';
import { h } from './dom.ts';
import { tierSelect } from './item-actions.ts';

export interface InboxHandlers {
  signIn(): void;
  answer(item: string, question: number, text: string, chosen: string | null): void;
  /** Accepts with the tier shown, in one action. A reject or close still asks first. */
  acceptNow(ids: string[], tier: number | null): void;
  confirm(kind: 'accept' | 'reject' | 'close', ids: string[]): void;
  flag(ids: string[], flagged: boolean): void;
}

const questionText = (item: ItemRow, s: Pick<ViewState, 'questions'>): string =>
  (item.question != null ? s.questions.get(item.question) : undefined) ??
  (item.needs === 'unclear' && item.unclear ? (UNCLEAR[item.unclear] ?? `Triage could not settle its ${item.unclear}.`) : 'The question’s text is stored outside the log. Open the item to read it.');

/** A button that a digit key presses while its row has focus. */
const keyed = (n: number, label: string, cls: string, onclick: () => void, extra: Record<string, string> = {}) =>
  h('button', { type: 'button', class: `button small ${cls}`, 'data-key': String(n), 'aria-keyshortcuts': String(n), onclick, ...extra }, h('kbd', {}, String(n)), ` ${label}`);

export function inboxActions(on: InboxHandlers) {
  const strips = new Map<string, { sig: string; el: HTMLElement }>();
  // Handlers read the newest state, not the one the strip was built from.
  let latest!: ViewState;

  const build = (item: ItemRow, s: ViewState, signedIn: boolean): HTMLElement => {
    const id = item.id;
    const kind = inboxKind(item)!;
    const busy = () => pending(latest.writes, id);
    const guard = (run: () => void) => () => {
      if (!busy()) run();
    };
    const status = h('p', { class: 'field-error', id: `inbox-error-${id}`, role: 'alert' });
    const body: (HTMLElement | null)[] = [];
    if (!signedIn) {
      body.push(h('button', { type: 'button', class: 'button small', 'data-focus': 'sign-in', onclick: on.signIn }, 'Sign in to act'));
    } else if (kind === 'question' && CAN.answer(item)) {
      const options = item.options ?? [];
      const question = item.question!;
      body.push(h('p', { class: 'inbox-question' }, questionText(item, s)));
      if (options.length) {
        body.push(h('div', { class: 'inbox-options', role: 'group', 'aria-label': 'Options' },
          ...options.slice(0, 9).map((o, i) => keyed(i + 1, o + (o === item.default ? ' (default)' : ''), o === item.default ? 'primary' : '', guard(() => on.answer(id, question, '', o)), { 'data-focus': `option-${i}` }))));
      }
      const text = h('input', { type: 'text', class: 'field', id: `inbox-text-${id}`, 'data-focus': 'answer-text', autocomplete: 'off', 'aria-describedby': `inbox-error-${id}` });
      const form = h('form', { class: 'inbox-answer' },
        h('label', { for: `inbox-text-${id}`, class: 'inline-label' }, options.length ? 'Or answer in words' : 'Answer'),
        text,
        h('button', { type: 'submit', class: 'button small', 'data-focus': 'answer-send' }, 'Send'));
      form.addEventListener('submit', (e) => {
        e.preventDefault();
        if (busy()) return;
        const problem = checkAnswer(options, text.value, null);
        status.textContent = problem ?? '';
        if (problem) {
          text.setAttribute('aria-invalid', 'true');
          return text.focus();
        }
        text.removeAttribute('aria-invalid');
        on.answer(id, question, text.value, null);
      });
      body.push(form);
    } else if ((kind === 'triage' || kind === 'review') && CAN.accept(item)) {
      const p = item.proposal;
      if (p?.why) body.push(h('p', { class: 'inbox-question' }, p.why));
      if (p?.depends_on.length) body.push(h('p', { class: 'muted small' }, `Waits on ${p.depends_on.join(', ')}`));
      if (p?.split.length) body.push(h('p', { class: 'muted small' }, `Split into ${p.split.join(', ')}`));
      const tier = tierSelect(`inbox-tier-${id}`, shownTier(item));
      tier.setAttribute('data-focus', 'tier');
      body.push(h('div', { class: 'inbox-row' },
        h('label', { for: `inbox-tier-${id}`, class: 'inline-label' }, 'Tier'),
        tier,
        keyed(1, 'Accept', 'primary', guard(() => on.acceptNow([id], tier.value === '' ? null : Number(tier.value))), { 'data-focus': 'accept' }),
        keyed(2, 'Reject…', 'danger', guard(() => on.confirm('reject', [id])), { 'data-focus': 'reject' })));
    } else {
      const actions: HTMLElement[] = [];
      if (CAN.unflag(item)) actions.push(keyed(actions.length + 1, 'Unflag', 'primary', guard(() => on.flag([id], false)), { 'data-focus': 'unflag' }));
      if (CAN.close(item)) actions.push(keyed(actions.length + 1, 'Close…', 'danger', guard(() => on.confirm('close', [id])), { 'data-focus': 'close' }));
      body.push(h('div', { class: 'inbox-row' }, ...actions, h('span', { class: 'muted small' }, 'Open the item for retries, notes and assigning.')));
    }
    return h('div', { class: 'inbox-act', 'data-kind': kind }, ...body, status);
  };

  return {
    /** The strip for `item`, rebuilt only when what it offers changes. */
    strip(item: ItemRow, s: ViewState): HTMLElement {
      latest = s;
      const signedIn = s.tracker.kind === 'signed-in';
      const sig = [signedIn, item.needs, item.stage, item.question, (item.options ?? []).join('\u0000'), item.default, item.proposed_tier, item.tier, item.flagged, item.proposal?.why].join('|');
      let kept = strips.get(item.id);
      if (!kept || kept.sig !== sig) {
        kept = { sig, el: build(item, s, signedIn) };
        strips.set(item.id, kept);
      }
      const busy = pending(s.writes, item.id);
      kept.el.toggleAttribute('aria-busy', busy);
      return kept.el;
    },
    /** Forgets strips of items no longer shown. */
    keep(ids: ReadonlySet<string>) {
      for (const id of strips.keys()) if (!ids.has(id)) strips.delete(id);
    },
  };
}
