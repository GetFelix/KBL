// The sign-in dialog and the confirm step before a write that cannot be undone
// (WCAG 3.3.4: the person reviews what will be written, and can cancel).
import type { ItemRow } from '../fold.ts';
import { acceptTier, assigneeAgent, assigneePerson, assignPick, checkPerson, CLOSE_WHY, shownTier, UNDO, type CloseWhy, type WriteArgs } from '../writes.ts';
import { modal } from './dialog.ts';
import { h } from './dom.ts';
import { tierSelect } from './item-actions.ts';

export type ConfirmKind = 'accept' | 'reject' | 'close';

const COPY: Record<ConfirmKind, { title: (n: string) => string; does: string; send: string; cls: string }> = {
  accept: { title: (n) => `Accept ${n}?`, does: 'Accepted work is published for agents to claim at the tier you pick.', send: 'Accept', cls: 'button primary' },
  reject: { title: (n) => `Reject ${n}?`, does: 'The proposal is rejected and no agent works on it.', send: 'Reject', cls: 'button primary danger-fill' },
  close: { title: (n) => `Close ${n}?`, does: 'The item is closed and its open children are cancelled.', send: 'Close', cls: 'button primary danger-fill' },
};

const SHOW = 8;

export function confirmDialog(send: (ids: string[], args: WriteArgs) => void) {
  const m = modal('Confirm', 'confirm-dialog');
  m.el.setAttribute('aria-labelledby', 'confirm-title');
  m.el.setAttribute('aria-describedby', 'confirm-does confirm-undo');
  m.el.removeAttribute('aria-label');
  return {
    el: m.el,
    isOpen: m.isOpen,
    open(kind: ConfirmKind, list: ItemRow[]) {
      const c = COPY[kind];
      const name = list.length === 1 ? list[0].id : `${list.length} items`;
      // Several proposals may carry different tiers; by default each keeps its own.
      const tier = kind === 'accept' ? (list.length === 1 ? tierSelect('confirm-tier', shownTier(list[0]), 'None recorded: the fold uses 2') : tierSelect('confirm-tier', null, 'Keep each item’s tier')) : null;
      const why = kind === 'close' ? h('select', { id: 'confirm-why', class: 'field select' }, ...CLOSE_WHY.map(([v, label]) => h('option', { value: v }, label))) : null;
      const go = h('button', { type: 'submit', class: c.cls }, c.send);
      const cancel = h('button', { type: 'button', class: 'button ghost', onclick: () => m.close() }, 'Cancel');
      const form = h('form', { method: 'dialog' },
        h('h2', { id: 'confirm-title' }, c.title(name)),
        list.length > 1
          ? h('ul', { class: 'confirm-items' },
              ...list.slice(0, SHOW).map((i) => h('li', {}, h('span', { class: 'row-id' }, i.id), ' ', i.title)),
              list.length > SHOW ? h('li', { class: 'muted' }, `and ${list.length - SHOW} more`) : null)
          : h('p', { class: 'muted' }, list[0]?.title ?? ''),
        h('p', { id: 'confirm-does' }, c.does),
        tier ? h('label', { for: 'confirm-tier', class: 'field-label' }, 'Tier') : null,
        tier,
        why ? h('label', { for: 'confirm-why', class: 'field-label' }, 'Why') : null,
        why,
        h('p', { id: 'confirm-undo', class: 'muted small' }, `This cannot be undone. ${UNDO[kind]}`),
        h('div', { class: 'dialog-actions' },
          h('span', { class: 'spacer' }),
          cancel,
          go),
      );
      form.addEventListener('submit', (e) => {
        e.preventDefault();
        m.close();
        const ids = list.map((i) => i.id);
        // A tier shown in the confirm is sent as shown; the fold's own pick applies only when none was shown.
        if (kind === 'accept') send(ids, { kind, tier: acceptTier(tier!.value) });
        else if (kind === 'close') send(ids, { kind, why: why!.value as CloseWhy });
        else send(ids, { kind });
      });
      m.body.replaceChildren(form);
      m.open();
      // Never on the send button, so a stray Enter cannot send a write that has no undo.
      (tier ?? why ?? cancel).focus();
    },
  };
}

/** Asks for the viewer's person token. `check` answers null when the tracker took it, else its reason. */
export function signInDialog(check: (token: string) => Promise<string | null>) {
  const m = modal('Sign in to the tracker', 'sign-in-dialog');
  m.el.setAttribute('aria-labelledby', 'sign-in-title');
  m.el.removeAttribute('aria-label');
  const input = h('input', { id: 'tracker-token', type: 'password', class: 'field', autocomplete: 'off', spellcheck: 'false', 'aria-required': 'true', 'aria-describedby': 'tracker-token-help tracker-token-error' });
  const error = h('p', { id: 'tracker-token-error', class: 'field-error' });
  const go = h('button', { type: 'submit', class: 'button primary' }, 'Sign in');
  const form = h('form', { method: 'dialog' },
    h('h2', { id: 'sign-in-title' }, 'Sign in to the tracker'),
    h('label', { for: 'tracker-token', class: 'field-label' }, 'Person token'),
    input,
    h('p', { id: 'tracker-token-help', class: 'muted small' },
      'Your own token from the tracker. Writes are made as you, with what the token allows. The page keeps it in this tab only and sends it only to its own /v1 routes.'),
    error,
    h('div', { class: 'dialog-actions' },
      h('span', { class: 'spacer' }),
      h('button', { type: 'button', class: 'button ghost', onclick: () => m.close() }, 'Cancel'),
      go),
  );
  m.body.append(form);
  let busy = false;
  form.addEventListener('submit', async (e) => {
    e.preventDefault();
    if (busy) return;
    const token = input.value.trim();
    if (!token) {
      error.textContent = 'Paste your person token.';
      input.setAttribute('aria-invalid', 'true');
      return input.focus();
    }
    busy = true;
    go.setAttribute('aria-disabled', 'true');
    go.textContent = 'Checking…';
    const problem = await check(token);
    busy = false;
    go.removeAttribute('aria-disabled');
    go.textContent = 'Sign in';
    error.textContent = problem ?? '';
    if (problem) {
      input.setAttribute('aria-invalid', 'true');
      return input.focus();
    }
    input.removeAttribute('aria-invalid');
    input.value = '';
    m.close();
  });
  return {
    el: m.el,
    isOpen: m.isOpen,
    open() {
      error.textContent = '';
      input.removeAttribute('aria-invalid');
      m.open();
      input.focus();
    },
  };
}

/** The assign picker: me, someone by name, or nobody. `names` are people the log already shows as assignees. */
export function assignDialog(send: (ids: string[], args: WriteArgs) => void) {
  const m = modal('Assign', 'assign-dialog');
  m.el.setAttribute('aria-labelledby', 'assign-title');
  m.el.removeAttribute('aria-label');
  return {
    el: m.el,
    isOpen: m.isOpen,
    open(item: ItemRow, me: string, names: readonly string[], agents: Promise<readonly string[]>) {
      const now = assigneePerson(item);
      const nowAgent = assigneeAgent(item);
      const pick = (args: WriteArgs | null) => {
        if (!args) return;
        m.close();
        send([item.id], args);
      };
      const meButton = h('button', { type: 'button', class: 'button primary', 'data-focus': 'assign-me', onclick: () => pick(assignPick(now, me, 'me')) }, `Me (${me})`);
      const nobody = h('button', { type: 'button', class: 'button', 'data-focus': 'assign-nobody', onclick: () => pick(nowAgent === null ? assignPick(now, me, 'nobody') : { kind: 'reassign', to: null }) }, 'Nobody');
      if (now === me) meButton.setAttribute('aria-disabled', 'true');
      if (now === null && nowAgent === null) nobody.setAttribute('aria-disabled', 'true');
      const agentRow = h('div', { class: 'action-row', role: 'group', 'aria-labelledby': 'assign-agents' }, h('span', { class: 'muted small' }, 'Loading agents…'));
      const agentPart = h('div', {}, h('p', { id: 'assign-agents', class: 'field-label' }, 'An agent'), agentRow);
      void agents.then((list) => {
        if (!agentPart.isConnected) return;
        if (list.length === 0) return agentPart.remove();
        agentRow.replaceChildren(
          ...list.map((a) => {
            const b = h('button', { type: 'button', class: 'button', onclick: () => (a === nowAgent ? undefined : pick({ kind: 'reassign', to: a, agent: true })) }, a);
            if (a === nowAgent) b.setAttribute('aria-disabled', 'true');
            return b;
          }),
        );
      });
      const others = names.filter((n) => n !== me);
      const input = h('input', { id: 'assign-name', type: 'text', class: 'field', list: 'assign-names', autocomplete: 'off', spellcheck: 'false', 'aria-describedby': 'assign-error' });
      const error = h('p', { id: 'assign-error', class: 'field-error' });
      const form = h('form', { method: 'dialog' },
        h('h2', { id: 'assign-title' }, `Assign ${item.id}`),
        h('p', { class: 'muted' }, now ? `${item.title} (now ${now})` : nowAgent ? `${item.title} (now agent ${nowAgent})` : `${item.title} (nobody yet)`),
        h('div', { class: 'action-row' }, meButton, nobody),
        agentPart,
        h('label', { for: 'assign-name', class: 'field-label' }, 'Someone else, by name'),
        input,
        h('datalist', { id: 'assign-names' }, ...others.map((n) => h('option', { value: n }))),
        h('p', { class: 'muted small' }, 'An item assigned to a person shows only in their queue, and no agent claims it. One assigned to an agent goes to that agent only.'),
        error,
        h('div', { class: 'dialog-actions' },
          h('span', { class: 'spacer' }),
          h('button', { type: 'button', class: 'button ghost', onclick: () => m.close() }, 'Cancel'),
          h('button', { type: 'submit', class: 'button' }, 'Assign')),
      );
      form.addEventListener('submit', (e) => {
        e.preventDefault();
        const problem = checkPerson(input.value);
        error.textContent = problem ?? '';
        if (problem) {
          input.setAttribute('aria-invalid', 'true');
          return input.focus();
        }
        pick({ kind: 'reassign', to: input.value.trim() });
      });
      m.body.replaceChildren(form);
      m.open();
      meButton.focus();
    },
  };
}
