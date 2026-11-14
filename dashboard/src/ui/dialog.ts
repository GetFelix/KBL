// A modal built on the native <dialog>: showModal() makes the rest of the page
// inert, which traps focus, and Escape closes it. Focus goes back to whatever had it
// before, so closing never strands a keyboard user at the top of the page.
import { h } from './dom.ts';

export interface Modal {
  el: HTMLDialogElement;
  body: HTMLElement;
  open(): void;
  close(): void;
  isOpen(): boolean;
}

export function modal(label: string, className: string, onClose: () => void = () => {}): Modal {
  const body = h('div', { class: 'dialog-body' });
  const el = h('dialog', { class: `dialog ${className}`, 'aria-label': label }, body);
  let returnTo: HTMLElement | null = null;
  el.addEventListener('close', () => {
    onClose();
    returnTo?.focus();
    returnTo = null;
  });
  // A click on the backdrop lands on the dialog element itself.
  el.addEventListener('click', (e) => {
    if (e.target === el) el.close();
  });
  return {
    el,
    body,
    open() {
      if (el.open) return;
      returnTo = document.activeElement instanceof HTMLElement ? document.activeElement : null;
      el.showModal();
    },
    close: () => el.close(),
    isOpen: () => el.open,
  };
}
