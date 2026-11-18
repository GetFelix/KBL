// The panel's Now line: one sentence on who holds the item and how recently it moved, or why
// it is not moving. It re-reads the clock on a timer, so "last activity 40 s ago" stays true.
import { nowLine, type ActivityEvent } from '../activity.ts';
import type { ItemRow } from '../fold.ts';
import type { ViewState } from '../state.ts';
import { h } from './dom.ts';

export interface NowSection {
  el: HTMLElement;
  /** Rewrites the sentence from the clock without touching anything else. */
  tick(): void;
  /** Forgets the item, so the clock never rewrites the sentence of one that is no longer open. */
  clear(): void;
  render(item: ItemRow, s: ViewState, events: readonly ActivityEvent[]): void;
}

export function nowSection(): NowSection {
  const text = h('p', { class: 'now-text' });
  const el = h('section', { class: 'now', 'aria-labelledby': 'now-title' }, h('h3', { id: 'now-title', class: 'section-title' }, 'Now'), text);
  let last: (() => void) | null = null;
  return {
    el,
    tick: () => last?.(),
    clear: () => { last = null; },
    render(item, s, events) {
      last = () => {
        const line = nowLine(item, s.blocked, events, Date.now());
        if (text.textContent !== line.text) text.textContent = line.text;
        el.dataset.tone = line.tone;
      };
      last();
    },
  };
}
