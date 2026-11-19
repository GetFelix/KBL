// The sign-in dialog and the confirm step before a write that cannot be undone
// (WCAG 3.3.4: the person reviews what will be written, and can cancel).
import type { ItemRow } from '../fold.ts';
import type { DecisionLine } from '../story.ts';
import { acceptTier, assigneeAgent, assigneePerson, assignPick, checkNeeds, checkPerson, checkRedo, CLOSE_WHY, needsOf, needsPick, redoCopy, redoNeedsNote, shownTier, UNDO, type CloseWhy, type WriteArgs } from '../writes.ts';
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

/**
 * The needs dialog: what the item's work needs, as words. Saving an empty field clears them.
 * `known` are needs the log already shows, offered as suggestions; the tracker refuses a need
 * outside the team's vocabulary and the refusal is shown with the write.
 */
export function needsDialog(send: (ids: string[], args: WriteArgs) => void) {
  const m = modal('Needs', 'needs-dialog');
  m.el.setAttribute('aria-labelledby', 'needs-title');
  m.el.removeAttribute('aria-label');
  return {
    el: m.el,
    isOpen: m.isOpen,
    open(item: ItemRow, known: readonly string[]) {
      const now = needsOf(item);
      const input = h('input', { id: 'needs-text', type: 'text', class: 'field', list: 'needs-known', autocomplete: 'off', spellcheck: 'false', value: now.join(', '), 'aria-describedby': 'needs-help needs-error' });
      const error = h('p', { id: 'needs-error', class: 'field-error' });
      const apply = (text: string) => {
        const problem = checkNeeds(text);
        error.textContent = problem ?? '';
        if (problem) {
          input.setAttribute('aria-invalid', 'true');
          return input.focus();
        }
        const args = needsPick(now, text);
        if (!args) {
          error.textContent = 'That is what it needs already.';
          return input.focus();
        }
        m.close();
        send([item.id], args);
      };
      const clear = h('button', { type: 'button', class: 'button danger', 'data-focus': 'needs-clear', onclick: () => apply('') }, 'Clear needs');
      if (now.length === 0) clear.setAttribute('aria-disabled', 'true');
      const form = h('form', { method: 'dialog' },
        h('h2', { id: 'needs-title' }, `What ${item.id} needs`),
        h('p', { class: 'muted' }, now.length ? `${item.title} (needs ${now.join(', ')})` : `${item.title} (needs nothing yet)`),
        h('label', { for: 'needs-text', class: 'field-label' }, 'Needs, separated by commas'),
        input,
        h('datalist', { id: 'needs-known' }, ...known.map((n) => h('option', { value: n }))),
        h('p', { id: 'needs-help', class: 'muted small' }, 'Only an agent that declares every one of these is handed the item and its fix, test and resume work. A list you set replaces what triage or an intake rule recorded.'),
        error,
        h('div', { class: 'dialog-actions' },
          clear,
          h('span', { class: 'spacer' }),
          h('button', { type: 'button', class: 'button ghost', onclick: () => m.close() }, 'Cancel'),
          h('button', { type: 'submit', class: 'button' }, 'Set needs')),
      );
      form.addEventListener('submit', (e) => {
        e.preventDefault();
        apply(input.value);
      });
      m.body.replaceChildren(form);
      m.open();
      input.focus();
    },
  };
}

/**
 * The redo confirm: names the decision it branches from, says what happens and what it costs,
 * and takes an optional note (required while a check is red). `reason` set means the redo is
 * not possible now; the dialog then only says why.
 */
export function redoDialog(send: (ids: string[], args: WriteArgs) => void) {
  const m = modal('Redo from a decision', 'redo-dialog');
  m.el.setAttribute('aria-labelledby', 'redo-title');
  m.el.removeAttribute('aria-label');
  return {
    el: m.el,
    isOpen: m.isOpen,
    open(item: ItemRow, d: DecisionLine | null, reason: string | null) {
      const cancel = h('button', { type: 'button', class: 'button ghost', onclick: () => m.close() }, reason ? 'Close' : 'Cancel');
      if (reason || !d) {
        m.body.replaceChildren(h('form', { method: 'dialog' },
          h('h2', { id: 'redo-title' }, `Cannot redo ${item.id} now`),
          h('p', { id: 'redo-does', role: 'alert' }, reason ?? 'There is no decision to redo from.'),
          h('div', { class: 'dialog-actions' }, h('span', { class: 'spacer' }), cancel)));
        m.open();
        return cancel.focus();
      }
      const c = redoCopy(item, d);
      const needsNote = redoNeedsNote(item);
      const note = h('textarea', { id: 'redo-note', class: 'field textarea', 'data-focus': 'redo-note', rows: '3', 'aria-describedby': 'redo-note-help redo-error', 'aria-required': String(needsNote) });
      const error = h('p', { id: 'redo-error', class: 'field-error', role: 'alert' });
      const form = h('form', { method: 'dialog' },
        h('h2', { id: 'redo-title' }, c.title),
        h('p', { class: 'muted' }, item.title),
        h('p', { id: 'redo-does' }, c.does),
        h('p', { id: 'redo-budget' }, c.budget),
        h('label', { for: 'redo-note', class: 'field-label' }, needsNote ? 'Note (required)' : 'Note (optional)'),
        note,
        h('p', { id: 'redo-note-help', class: 'muted small' }, needsNote
          ? 'A check failed on the pull request. Say what the new attempt should do differently; it reads this note first.'
          : 'The new attempt reads this note first. It is saved on the item as your note.'),
        error,
        h('p', { class: 'muted small' }, 'This cannot be undone.'),
        h('div', { class: 'dialog-actions' }, h('span', { class: 'spacer' }), cancel, h('button', { type: 'submit', class: 'button primary' }, 'Start redo')));
      form.addEventListener('submit', (e) => {
        e.preventDefault();
        const problem = checkRedo(note.value, needsNote);
        error.textContent = problem ?? '';
        if (problem) {
          note.setAttribute('aria-invalid', 'true');
          return note.focus();
        }
        m.close();
        send([item.id], { kind: 'redo', fromAttempt: d.attempt, at: d.offset, note: note.value });
      });
      m.body.replaceChildren(form);
      m.open();
      note.focus();
    },
  };
}
