// The `?` overlay: every shortcut in the registry, grouped. Palette-only commands
// are listed too, so the overlay is the whole map.
import { COMMANDS, displayChord } from '../keys.ts';
import { modal } from './dialog.ts';
import { h, kbd } from './dom.ts';

export function helpView(mac: boolean) {
  const m = modal('Keyboard shortcuts', 'help');
  const groups = [...new Set(COMMANDS.map((c) => c.group))];
  const close = h('button', { type: 'button', class: 'button ghost', onclick: () => m.close() }, 'Close');
  m.body.append(
    h('header', { class: 'dialog-head' }, h('h2', {}, 'Keyboard shortcuts'), close),
    ...groups.map((g) =>
      h('section', {},
        h('h3', {}, g),
        h('dl', { class: 'shortcuts' },
          ...COMMANDS.filter((c) => c.group === g).flatMap((c) => [
            h('dt', {}, c.label),
            h('dd', {}, ...(c.keys.length ? c.keys.map((k) => kbd(displayChord(k, mac))) : [h('span', { class: 'muted' }, 'Palette')])),
          ]),
        ),
      ),
    ),
  );
  return {
    el: m.el,
    isOpen: m.isOpen,
    open() {
      m.open();
      close.focus();
    },
  };
}
