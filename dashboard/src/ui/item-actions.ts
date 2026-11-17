// The open item's actions in the panel: accept, reject, tier, close and the answer form.
// Built once per item and the set of actions it offers, then updated in place, so a live
// record never wipes a half-typed answer or takes the focus off a button.
import type { ItemRow } from '../fold.ts';
import type { ViewState } from '../state.ts';
import { CAN, canAssign, canSteer, canTakeAfterLimit, checkAnswer, checkSteer, heldAttempt, latestFor, pending, shownTier, UNCLEAR, type WriteKind } from '../writes.ts';
import { byFocus, h } from './dom.ts';

export interface WriteHandlers {
  signIn(): void;
  /** Asks to confirm, then sends. `ids` is one item from the panel or the marked items. */
  confirm(kind: 'accept' | 'reject' | 'close', ids: string[]): void;
  setTier(ids: string[], tier: number): void;
  /** Flags the items for a person, or clears the flag. Neither asks first: each takes the other back. */
  flag(ids: string[], flagged: boolean): void;
  /** Marks the summary of an attempt accurate or not. The other mark replaces it, so it asks nothing first. */
  rate(item: string, attempt: number, accurate: boolean): void;
  /** Assigns the item to the signed-in person. */
  take(item: string): void;
  /** Opens the assign picker for the item. */
  assign(item: string): void;
  /** Sends a note to the agent holding `attempt`. */
  steer(item: string, attempt: number, text: string): void;
  answer(item: string, question: number, text: string, chosen: string | null): void;
  undo(): void;
  dismiss(): void;
  clearMarks(): void;
}

export const TIERS = [
  [0, '0, most urgent'],
  [1, '1'],
  [2, '2'],
  [3, '3, least urgent'],
] as const;

/** A tier field starting at `current`. With none, it starts on an empty option labelled `none`, never on a tier nobody recorded. */
export function tierSelect(id: string, current: number | null, none = 'No tier recorded'): HTMLSelectElement {
  const sel = h('select', { id, class: 'field select', 'data-focus': id });
  if (current === null) sel.append(h('option', { value: '' }, none));
  for (const [t, label] of TIERS) {
    const o = h('option', { value: String(t) }, label);
    o.selected = t === current;
    sel.append(o);
  }
  if (current === null) sel.value = '';
  return sel;
}

const PENDING: Record<WriteKind, string> = {
  accept: 'Accepting…',
  reject: 'Rejecting…',
  tier: 'Setting the tier…',
  close: 'Closing…',
  answer: 'Sending the answer…',
  flag: 'Flagging…',
  unflag: 'Unflagging…',
  rate: 'Marking the summary…',
  take: 'Taking…',
  reassign: 'Assigning…',
  steer: 'Sending the note…',
  undo: 'Undoing…',
};

export function itemActions(on: WriteHandlers) {
  const el = h('section', { class: 'actions', 'aria-labelledby': 'actions-title' });
  let built = '';
  let auth = '';
  let tier: HTMLSelectElement | null = null;
  let status: HTMLElement = h('p');
  const guard = (s: () => ViewState, item: string, run: () => void) => () => {
    if (!pending(s().writes, item)) run();
  };
  let current: () => ViewState = () => {
    throw new Error('rendered before a state');
  };

  const answerForm = (item: ItemRow, s: ViewState): HTMLFormElement => {
    const options = item.options ?? [];
    const question = item.question!;
    const asked = s.questions.get(question);
    const text = h('textarea', { id: 'answer-text', class: 'field textarea', 'data-focus': 'answer-text', rows: '3', 'aria-describedby': 'answer-help answer-error' });
    const error = h('p', { id: 'answer-error', class: 'field-error' });
    const radios = options.map((o, i) =>
      h('label', { class: 'choice' },
        h('input', { type: 'radio', name: 'answer-choice', value: o, 'data-focus': `choice-${i}` }),
        o,
        o === item.default ? h('span', { class: 'muted' }, ' (the agent’s default)') : null));
    const form = h('form', { class: 'answer', 'aria-labelledby': 'answer-title' },
      h('h4', { id: 'answer-title' }, 'Answer the question'),
      // Triage's question has no text: what it could not settle is the question.
      h('p', { class: 'question' }, asked ?? (item.needs === 'unclear' && item.unclear ? (UNCLEAR[item.unclear] ?? `Triage could not settle its ${item.unclear}.`) : 'The question’s text is stored as an artifact, outside the log.')),
      radios.length ? h('fieldset', {}, h('legend', { class: 'field-label' }, 'Options'), ...radios) : null,
      h('label', { for: 'answer-text', class: 'field-label' }, radios.length ? 'Or answer in words instead' : 'Answer'),
      text,
      h('p', { id: 'answer-help', class: 'muted small' }, 'Sending cannot be undone: the answer starts the work that resumes from it.'),
      error,
      h('button', { type: 'submit', class: 'button primary', 'data-focus': 'answer-send' }, 'Send answer'),
    );
    form.addEventListener('submit', (e) => {
      e.preventDefault();
      if (pending(current().writes, item.id)) return;
      const chosen = form.querySelector<HTMLInputElement>('input[name=answer-choice]:checked')?.value ?? null;
      const problem = checkAnswer(options, text.value, chosen);
      error.textContent = problem ?? '';
      if (problem) {
        text.setAttribute('aria-invalid', 'true');
        return text.focus();
      }
      text.removeAttribute('aria-invalid');
      on.answer(item.id, question, text.value, chosen);
    });
    return form;
  };

  const steerForm = (item: ItemRow, attempt: number): HTMLFormElement => {
    const text = h('textarea', { id: 'steer-text', class: 'field textarea', 'data-focus': 'steer-text', rows: '2', 'aria-describedby': 'steer-help steer-error' });
    const error = h('p', { id: 'steer-error', class: 'field-error' });
    const form = h('form', { class: 'answer', 'aria-labelledby': 'steer-title' },
      h('h4', { id: 'steer-title' }, 'Send to the agent'),
      h('label', { for: 'steer-text', class: 'field-label' }, 'Note'),
      text,
      h('p', { id: 'steer-help', class: 'muted small' }, 'The agent working this item reads the note on its next step. It cannot be taken back.'),
      error,
      h('button', { type: 'submit', class: 'button', 'data-focus': 'steer-send' }, 'Send to agent'),
    );
    form.addEventListener('submit', (e) => {
      e.preventDefault();
      if (pending(current().writes, item.id)) return;
      const problem = checkSteer(text.value);
      error.textContent = problem ?? '';
      if (problem) {
        text.setAttribute('aria-invalid', 'true');
        return text.focus();
      }
      text.removeAttribute('aria-invalid');
      on.steer(item.id, attempt, text.value);
      text.value = '';
    });
    return form;
  };

  return {
    el,
    /** Draws the actions for `item`, or nothing when it offers none. */
    render(item: ItemRow, s: ViewState): HTMLElement {
      current = () => s;
      const kinds = (Object.keys(CAN) as (keyof typeof CAN)[]).filter((k) => CAN[k](item));
      const assignable = canAssign(item);
      const limit = canTakeAfterLimit(item);
      const steerAt = canSteer(item) ? heldAttempt(item, s.stories.get(item.id)) : null;
      // `checking` keeps the last sign-in state, so checking a token does not rebuild the form.
      if (s.tracker.kind !== 'checking') auth = s.tracker.kind === 'refused' ? `refused:${s.tracker.reason}` : s.tracker.kind;
      const key = `${item.id}|${kinds.join(',')}|${assignable}|${limit}|${steerAt}|${auth}|${item.question}|${item.unclear}|${(item.options ?? []).join('\u0000')}|${item.question == null ? '' : s.questions.get(item.question)}`;
      if (key !== built) {
        built = key;
        const hadFocus = el.contains(document.activeElement) ? ((document.activeElement as HTMLElement).dataset.focus ?? '') : null;
        const head = h('h3', { id: 'actions-title', class: 'actions-title', tabindex: '-1' }, 'Actions');
        status = h('p', { class: 'write-status' });
        tier = null;
        if (kinds.length === 0 && !assignable && steerAt === null) {
          el.replaceChildren(head, h('p', { class: 'muted' }, 'Nothing to do here: the item is closed.'));
        } else if (s.tracker.kind !== 'signed-in') {
          el.replaceChildren(head,
            h('p', { class: 'muted' }, s.tracker.kind === 'refused' ? `Sign in again: ${s.tracker.reason}` : 'Sign in to the tracker to act on this item.'),
            h('button', { type: 'button', class: 'button', 'data-focus': 'sign-in', onclick: on.signIn }, 'Sign in'));
        } else {
          const id = item.id;
          const button = (kind: 'accept' | 'reject' | 'close', label: string, cls = 'button') =>
            kinds.includes(kind) ? h('button', { type: 'button', class: cls, 'data-focus': kind, onclick: guard(() => current(), id, () => on.confirm(kind, [id])) }, label) : null;
          if (kinds.includes('tier')) tier = tierSelect('panel-tier', shownTier(item));
          const parts: (HTMLElement | null)[] = [head,
            h('div', { class: 'action-row' }, button('accept', 'Accept…', 'button primary'), button('reject', 'Reject…', 'button danger'), button('close', 'Close…', 'button danger'),
              ...(['flag', 'unflag'] as const).map((k) => kinds.includes(k)
                ? h('button', { type: 'button', class: 'button', 'data-focus': k, title: k === 'flag' ? 'Move it to Needs you and keep agents off it' : 'Take it out of Needs you', onclick: guard(() => current(), id, () => on.flag([id], k === 'flag')) }, k === 'flag' ? 'Flag for a person' : 'Unflag')
                : null)),
            assignable
              ? h('div', { class: 'action-row' },
                  limit ? h('button', { type: 'button', class: 'button primary', 'data-focus': 'take', title: 'Assign it to yourself; no agent will claim it', onclick: guard(() => current(), id, () => on.take(id)) }, 'Take it yourself') : null,
                  h('button', { type: 'button', class: 'button', 'data-focus': 'assign', title: 'Pick who is doing this', onclick: guard(() => current(), id, () => on.assign(id)) }, 'Assign…'))
              : null,
            steerAt !== null ? steerForm(item, steerAt) : null,
            tier
              ? h('div', { class: 'action-row' },
                  h('label', { for: 'panel-tier', class: 'inline-label' }, 'Tier'),
                  tier,
                  h('button', { type: 'button', class: 'button', 'data-focus': 'set-tier', onclick: guard(() => current(), id, () => {
                    if (tier!.value === '') {
                      status.textContent = 'Pick a tier first.';
                      return tier!.focus();
                    }
                    on.setTier([id], Number(tier!.value));
                  }) }, 'Set tier'))
              : null,
            kinds.includes('answer') ? answerForm(item, s) : null,
            status];
          el.replaceChildren(...parts.filter((p): p is HTMLElement => p !== null));
        }
        // The control that had the focus is gone (the item moved stage, say); keep it in the actions.
        if (hadFocus !== null && !el.contains(document.activeElement)) {
          // Flag and Unflag swap places: the focus follows the toggle.
          const toggled = ({ flag: 'unflag', unflag: 'flag' } as Record<string, string>)[hadFocus] ?? hadFocus;
          const same = hadFocus ? (byFocus(el, hadFocus) ?? byFocus(el, toggled)) : null;
          (same ?? head).focus({ preventScroll: true });
        }
      }
      // In place: what the last write on this item did, and whether one is in flight.
      const busy = pending(s.writes, item.id);
      el.toggleAttribute('aria-busy', busy);
      for (const b of Array.from(el.querySelectorAll<HTMLButtonElement>('button:not([data-focus=sign-in])'))) {
        // aria-disabled, not disabled: a disabled button drops the focus to the page.
        if (busy) b.setAttribute('aria-disabled', 'true');
        else b.removeAttribute('aria-disabled');
      }
      const shown = shownTier(item);
      if (tier && document.activeElement !== tier && !busy && shown != null && tier.value !== String(shown)) tier.value = String(shown);
      const last = latestFor(s.writes, item.id);
      const text = !last ? '' : last.status === 'pending' ? PENDING[last.kind] : last.status === 'ok' ? '' : `${last.status === 'refused' ? 'The tracker refused it' : 'Not sent'}: ${last.reason}${last.why ? ` (${last.why})` : ''}`;
      if (status.textContent !== text) status.textContent = text;
      status.classList.toggle('error', !!last && (last.status === 'refused' || last.status === 'failed'));
      return el;
    },
  };
}
