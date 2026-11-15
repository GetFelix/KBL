// The one shortcut registry. The key handler, the command palette and the `?`
// overlay all read it, so a key cannot be listed in one place and work differently
// in another. Keys follow Linear and GitHub (docs/design/dashboard-ux.md).

export type CommandId =
  | 'palette'
  | 'help'
  | 'spend'
  | 'search'
  | 'next'
  | 'previous'
  | 'open'
  | 'close'
  | 'first'
  | 'last'
  | 'left'
  | 'right'
  | 'layout'
  | 'filter'
  | 'clearFilters'
  | 'saveView'
  | 'density'
  | 'theme'
  | 'view-all'
  | 'view-inbox'
  | 'view-blocked'
  | 'mark'
  | 'markAll'
  | 'clearMarks'
  | 'accept'
  | 'closeItems'
  | 'flag'
  | 'unflag'
  | 'undo'
  | 'signIn';

export interface Command {
  id: CommandId;
  /** What the palette and the overlay show. */
  label: string;
  group: 'Navigation' | 'View' | 'Actions' | 'General';
  /** Key chords as shown to people: `Mod` is Cmd on a Mac and Ctrl elsewhere. Empty for palette-only commands. */
  keys: string[];
}

export const COMMANDS: readonly Command[] = [
  { id: 'palette', label: 'Open the command palette', group: 'General', keys: ['Mod+K'] },
  { id: 'help', label: 'Show keyboard shortcuts', group: 'General', keys: ['?'] },
  { id: 'search', label: 'Search items', group: 'Navigation', keys: ['/'] },
  { id: 'next', label: 'Next item', group: 'Navigation', keys: ['J', 'ArrowDown'] },
  { id: 'previous', label: 'Previous item', group: 'Navigation', keys: ['K', 'ArrowUp'] },
  { id: 'first', label: 'First item', group: 'Navigation', keys: ['Home'] },
  { id: 'last', label: 'Last item', group: 'Navigation', keys: ['End'] },
  { id: 'left', label: 'Previous column on the board', group: 'Navigation', keys: ['H', 'ArrowLeft'] },
  { id: 'right', label: 'Next column on the board', group: 'Navigation', keys: ['L', 'ArrowRight'] },
  { id: 'open', label: 'Open the selected item', group: 'Navigation', keys: ['Enter'] },
  { id: 'close', label: 'Close, or clear the search', group: 'Navigation', keys: ['Escape'] },
  { id: 'spend', label: 'Show spend', group: 'View', keys: [] },
  { id: 'view-all', label: 'Show all work', group: 'View', keys: [] },
  { id: 'view-inbox', label: 'Show what needs you', group: 'View', keys: [] },
  { id: 'view-blocked', label: 'Show blocked work', group: 'View', keys: [] },
  { id: 'layout', label: 'Switch list and board', group: 'View', keys: ['V'] },
  { id: 'filter', label: 'Filter items', group: 'View', keys: ['F'] },
  { id: 'clearFilters', label: 'Clear all filters', group: 'View', keys: [] },
  { id: 'saveView', label: 'Save this view', group: 'View', keys: [] },
  { id: 'density', label: 'Switch comfortable and compact density', group: 'View', keys: [] },
  { id: 'theme', label: 'Switch light and dark theme', group: 'View', keys: [] },
  { id: 'mark', label: 'Select or unselect the item', group: 'Actions', keys: ['X'] },
  { id: 'markAll', label: 'Select every item shown', group: 'Actions', keys: [] },
  { id: 'clearMarks', label: 'Clear the selection', group: 'Actions', keys: [] },
  { id: 'accept', label: 'Accept the selected items', group: 'Actions', keys: [] },
  { id: 'closeItems', label: 'Close the selected items', group: 'Actions', keys: [] },
  { id: 'flag', label: 'Flag the selected items for a person', group: 'Actions', keys: [] },
  { id: 'unflag', label: 'Unflag the selected items', group: 'Actions', keys: [] },
  { id: 'undo', label: 'Undo the last tier change', group: 'Actions', keys: ['Mod+Z'] },
  { id: 'signIn', label: 'Sign in to the tracker', group: 'Actions', keys: [] },
];

/** The first chord bound to a command, for labels outside the palette and the overlay. */
export function chordFor(id: CommandId): string {
  return COMMANDS.find((c) => c.id === id)?.keys[0] ?? '';
}

/** The parts of a KeyboardEvent the matcher reads, so tests need no DOM. */
export interface KeyPress {
  key: string;
  metaKey: boolean;
  ctrlKey: boolean;
  altKey: boolean;
  /** True when focus is in a text field, where plain letters must type. */
  inText: boolean;
}

function chordMatches(chord: string, press: KeyPress, mac: boolean): boolean {
  const parts = chord.split('+');
  const key = parts.pop() ?? '';
  const wantsMod = parts.includes('Mod');
  const mod = mac ? press.metaKey : press.ctrlKey;
  if (wantsMod !== mod || press.altKey) return false;
  // The other platform's modifier must be up, or Ctrl+K on a Mac would open the palette and eat Emacs keys.
  if (mac ? press.ctrlKey : press.metaKey) return false;
  return key.length === 1 ? press.key.toLowerCase() === key.toLowerCase() : press.key === key;
}

/** The command a key press runs, or undefined. Plain keys do nothing while typing, except Escape and arrows. */
export function commandFor(press: KeyPress, mac: boolean): CommandId | undefined {
  for (const c of COMMANDS) {
    for (const chord of c.keys) {
      const plain = !chord.includes('Mod+');
      if (press.inText && plain && !['Escape', 'ArrowDown', 'ArrowUp', 'Enter'].includes(chord)) continue;
      if (chordMatches(chord, press, mac)) return c.id;
    }
  }
  return undefined;
}

/** A chord as a person reads it on this platform: `Mod+K` becomes `⌘ K` or `Ctrl K`. */
export function displayChord(chord: string, mac: boolean): string[] {
  const names: Record<string, string> = { ArrowDown: '↓', ArrowUp: '↑', ArrowLeft: '←', ArrowRight: '→', Escape: 'Esc', Enter: 'Enter' };
  return chord.split('+').map((p) => (p === 'Mod' ? (mac ? '⌘' : 'Ctrl') : (names[p] ?? p)));
}
