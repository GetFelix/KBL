// A modal built on the native <dialog>: showModal() makes the rest of the page
// inert, which traps focus, and Escape closes it. Closing gives focus back to what
// had it before, so a keyboard user is never stranded at the top of the page.
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
  // The browser restores focus inside close(), so a command run after closing can move it on.
  el.addEventListener('close', onClose);
  // A click on the backdrop lands on the dialog element itself.
  el.addEventListener('click', (e) => {
    if (e.target === el) el.close();
  });
  return {
    el,
    body,
    open() {
      if (!el.open) el.showModal();
    },
    close: () => el.close(),
    isOpen: () => el.open,
  };
}
