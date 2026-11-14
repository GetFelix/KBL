// The Cmd+K palette: a combobox over every command in the registry and every item.
// The input keeps focus; arrows move aria-activedescendant through the listbox.
import { COMMANDS, displayChord, type CommandId } from '../keys.ts';
import { rank, type Entry } from '../palette.ts';
import { stageLabel } from '../list.ts';
import type { ViewState } from '../state.ts';
import { modal } from './dialog.ts';
import { h, kbd } from './dom.ts';

export interface PaletteHandlers {
  run(id: CommandId): void;
  openItem(id: string): void;
}

const MAX_ROWS = 50;

export function entries(s: ViewState): Entry[] {
  return [
    ...COMMANDS.filter((c) => c.id !== 'palette').map((c) => ({ key: `cmd:${c.id}`, label: c.label, section: 'Commands' })),
    ...s.items.map((i) => ({ key: `item:${i.id}`, label: i.title, hint: `${i.id} · ${stageLabel(i.stage)}`, section: 'Items' })),
  ];
}

export function paletteView(getState: () => ViewState, on: PaletteHandlers, mac: boolean) {
  const m = modal('Command palette', 'palette');
  const input = h('input', {
    type: 'text',
    class: 'palette-input',
    role: 'combobox',
    'aria-expanded': 'true',
    'aria-controls': 'palette-list',
    'aria-autocomplete': 'list',
    'aria-label': 'Type a command or search items',
    placeholder: 'Type a command or search items',
    autocomplete: 'off',
    spellcheck: 'false',
  });
  const list = h('ul', { id: 'palette-list', role: 'listbox', 'aria-label': 'Results' });
  const none = h('p', { class: 'muted palette-none', role: 'status' });
  m.body.append(input, list, none);
  let shown: Entry[] = [];
  let active = 0;

  const choose = (e: Entry | undefined) => {
    if (!e) return;
    m.close();
    const [kind, id] = [e.key.slice(0, e.key.indexOf(':')), e.key.slice(e.key.indexOf(':') + 1)];
    if (kind === 'cmd') on.run(id as CommandId);
    else on.openItem(id);
  };

  const paint = () => {
    shown = rank(entries(getState()), input.value).slice(0, MAX_ROWS);
    active = Math.min(active, Math.max(0, shown.length - 1));
    none.textContent = shown.length ? '' : `No command or item matches “${input.value}”.`;
    list.replaceChildren(
      ...shown.map((e, i) => {
        const cmd = COMMANDS.find((c) => `cmd:${c.id}` === e.key);
        return h('li', {
          id: `pal-${i}`,
          role: 'option',
          class: 'palette-row',
          'aria-selected': i === active ? 'true' : 'false',
          onclick: () => choose(e),
          onmousemove: () => {
            if (active !== i) {
              active = i;
              mark();
            }
          },
        },
          h('span', { class: 'palette-section' }, e.section === 'Items' ? 'Item' : 'Command'),
          h('span', { class: 'palette-label' }, e.label),
          e.hint ? h('span', { class: 'palette-hint' }, e.hint) : null,
          cmd?.keys[0] ? kbd(displayChord(cmd.keys[0], mac)) : null,
        );
      }),
    );
    mark();
  };
  const mark = () => {
    list.querySelectorAll('[role="option"]').forEach((li, i) => li.setAttribute('aria-selected', String(i === active)));
    if (shown.length) input.setAttribute('aria-activedescendant', `pal-${active}`);
    else input.removeAttribute('aria-activedescendant');
    list.querySelector(`#pal-${active}`)?.scrollIntoView({ block: 'nearest' });
  };

  input.addEventListener('input', () => {
    active = 0;
    paint();
  });
  input.addEventListener('keydown', (e) => {
    if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
      e.preventDefault();
      if (shown.length) active = (active + (e.key === 'ArrowDown' ? 1 : -1) + shown.length) % shown.length;
      mark();
    } else if (e.key === 'Enter') {
      e.preventDefault();
      choose(shown[active]);
    }
  });

  return {
    el: m.el,
    isOpen: m.isOpen,
    open(query = '') {
      input.value = query;
      active = 0;
      paint();
      m.open();
      input.focus();
    },
  };
}
