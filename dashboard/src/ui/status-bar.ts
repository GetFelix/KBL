// The connection line in the header. It is a polite live region, so a screen reader
// hears "Live" or "Disconnected" without the focus moving.
import { describe, type ViewState } from '../state.ts';
import { h } from './dom.ts';

const ICON = { ok: '●', busy: '◌', warn: '▲', error: '■' } as const;
// A live log changes the item count with every import; say the count once things settle.
const COUNT_SETTLE_MS = 1500;

export function statusBar(): { el: HTMLElement; render(s: ViewState): void } {
  const el = h('p', { class: 'status', role: 'status', 'aria-live': 'polite' });
  let last = '';
  let lastKind = '';
  let latest = { text: '', tone: 'ok' as keyof typeof ICON };
  let pending: ReturnType<typeof setTimeout> | undefined;
  const write = (text: string, tone: keyof typeof ICON) => {
    el.dataset.tone = tone;
    // The shape carries the tone too, so it does not rest on colour alone.
    el.replaceChildren(h('span', { class: 'status-icon', 'aria-hidden': 'true' }, ICON[tone]), text);
  };
  return {
    el,
    render(s) {
      const { text, tone } = describe(s.connection, s.items.length);
      if (text + tone === last) return;
      last = text + tone;
      latest = { text, tone };
      if (s.connection.kind === lastKind && s.connection.kind === 'live' && el.dataset.tone === tone) {
        // Throttle: a timer already pending is kept, so a steady import still updates, and it writes the latest text.
        pending ??= setTimeout(() => {
          pending = undefined;
          write(latest.text, latest.tone);
        }, COUNT_SETTLE_MS);
      } else {
        clearTimeout(pending);
        pending = undefined;
        write(text, tone);
      }
      lastKind = s.connection.kind;
    },
  };
}
