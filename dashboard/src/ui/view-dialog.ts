// Names a saved view: Save for a new one, or Rename and Delete for one in the sidebar.
// A name another view has is refused in words next to the field (WCAG 3.3.1), and
// Delete sits apart from Save so it is not hit by accident.
import { modal } from './dialog.ts';
import { h } from './dom.ts';

export interface ViewDialogHandlers {
  /** Returns an error to show, or null when the name was taken. */
  submit(name: string, editing: string | null): string | null;
  remove(name: string): void;
}

export function viewDialog(on: ViewDialogHandlers) {
  const m = modal('Saved view', 'view-dialog');
  const title = h('h2', { id: 'view-dialog-title' });
  const input = h('input', { id: 'view-name', type: 'text', class: 'field', autocomplete: 'off', required: true, 'aria-describedby': 'view-name-error' });
  const error = h('p', { id: 'view-name-error', class: 'field-error' });
  const remove = h('button', { type: 'button', class: 'button danger' }, 'Delete view');
  const form = h('form', { method: 'dialog' },
    h('label', { for: 'view-name', class: 'field-label' }, 'Name'),
    input,
    error,
    h('div', { class: 'dialog-actions' },
      remove,
      h('span', { class: 'spacer' }),
      h('button', { type: 'button', class: 'button ghost', onclick: () => m.close() }, 'Cancel'),
      h('button', { type: 'submit', class: 'button primary' }, 'Save'),
    ),
  );
  m.el.setAttribute('aria-labelledby', 'view-dialog-title');
  m.el.removeAttribute('aria-label');
  m.body.append(title, form);
  let editing: string | null = null;
  form.addEventListener('submit', (e) => {
    e.preventDefault();
    const problem = input.value.trim() ? on.submit(input.value, editing) : 'Give the view a name.';
    error.textContent = problem ?? '';
    input.toggleAttribute('aria-invalid', problem !== null);
    if (problem) input.focus();
    else m.close();
  });
  remove.addEventListener('click', () => {
    if (editing === null) return;
    m.close();
    on.remove(editing);
  });
  return {
    el: m.el,
    isOpen: m.isOpen,
    /** Opens for a new view, or for `name` to rename or delete. */
    open(name: string | null, suggestion = '') {
      editing = name;
      title.textContent = name === null ? 'Save this view' : `Edit “${name}”`;
      remove.hidden = name === null;
      input.value = name ?? suggestion;
      error.textContent = '';
      input.removeAttribute('aria-invalid');
      m.open();
      input.select();
    },
  };
}
