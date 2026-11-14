// The connection line in the header. It is a polite live region, so a screen reader
// hears "Live" or "Disconnected" without the focus moving.
import { describe, type ViewState } from '../state.ts';
import { h } from './dom.ts';

const ICON = { ok: '●', busy: '◌', warn: '▲', error: '■' } as const;

export function statusBar(): { el: HTMLElement; render(s: ViewState): void } {
  const el = h('p', { class: 'status', role: 'status', 'aria-live': 'polite' });
  let last = '';
  return {
    el,
    render(s) {
      const { text, tone } = describe(s.connection, s.items.length);
      if (text + tone === last) return;
      last = text + tone;
      el.dataset.tone = tone;
      // The shape carries the tone too, so it does not rest on colour alone.
      el.replaceChildren(h('span', { class: 'status-icon', 'aria-hidden': 'true' }, ICON[tone]), text);
    },
  };
}
