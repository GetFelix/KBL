// The panel's Activity: what the log recorded on the item, newest first. A step folds its
// output tail away; what a person opened stays open when a live record redraws the panel.
import { since, timeline, type ActivityEvent, type TimelineEntry } from '../activity.ts';
import { h } from './dom.ts';

const SHOWN = 30;

/** A relative time that keeps its absolute value in the title and `datetime`. */
export function timeEl(ms: number, now = Date.now()): HTMLTimeElement {
  const iso = new Date(ms).toISOString();
  const abs = iso.replace('T', ' ').replace('.000Z', ' UTC');
  // Focusable so the absolute time shows on focus as well as hover, and read out with the relative one.
  return h('time', { datetime: iso, title: abs, tabindex: '0', class: 'abs-time', 'data-abs': abs, 'data-ms': String(ms) },
    h('span', { class: 'rel' }, since(ms, now)),
    h('span', { class: 'sr-only' }, `, ${abs}`));
}

/** Rewrites every relative time under `root`. */
export function tickTimes(root: ParentNode, now = Date.now()): void {
  for (const t of Array.from(root.querySelectorAll<HTMLElement>('time[data-ms]'))) {
    const next = since(Number(t.dataset.ms), now);
    const rel = t.querySelector('.rel');
    if (rel && rel.textContent !== next) rel.textContent = next;
  }
}

export interface TimelineSection {
  /** Builds the list for this item's events. Keeps the folds a person opened and the "show all" choice; `redraw` asks the panel to build again. */
  render(item: string, events: readonly ActivityEvent[], redraw: () => void): HTMLElement;
}

export function timelineSection(): TimelineSection {
  let forItem = '';
  const opened = new Set<number>();
  let all = false;
  return {
    render(item, events, redraw) {
      if (item !== forItem) {
        forItem = item;
        opened.clear();
        all = false;
      }
      const entries = timeline(events);
      const shown = all ? entries : entries.slice(0, SHOWN);
      const line = (e: TimelineEntry): HTMLElement => {
        const out = e.output?.trim();
        return h('li', { class: `tl-entry${e.failed || e.refused ? ' tl-failed' : ''}${e.refused ? ' tl-refused' : ''}`, 'data-at': String(e.offset) },
          h('div', { class: 'tl-head' },
            h('strong', {}, e.title),
            h('span', { class: 'muted' }, ` ${e.who} `),
            timeEl(e.at_ms),
            e.refused ? h('span', { class: 'tl-flag' }, ' refused') : e.failed ? h('span', { class: 'tl-flag' }, ' failed') : null),
          e.detail ? h('p', { class: 'tl-detail' }, e.detail) : null,
          out
            ? h('details', {
                class: 'tl-output',
                open: opened.has(e.offset) ? true : undefined,
                ontoggle: (ev) => {
                  const d = ev.currentTarget as HTMLDetailsElement;
                  if (d.open) opened.add(e.offset);
                  else opened.delete(e.offset);
                },
              },
              h('summary', { 'data-focus': `out:${e.offset}` }, 'Output tail'),
              h('pre', { class: 'tl-pre' }, out))
            : null);
      };
      return h('section', { class: 'timeline', 'aria-labelledby': 'timeline-title' },
        h('h3', { id: 'timeline-title', class: 'section-title', tabindex: '-1', 'data-focus': 'tl-title' }, 'Activity'),
        entries.length === 0
          ? h('p', { class: 'muted' }, 'Nothing recorded on this item yet.')
          : h('ol', { class: 'tl' }, ...shown.map(line)),
        entries.length > SHOWN && !all
          ? h('button', { type: 'button', class: 'button', 'data-focus': 'tl-all', 'data-focus-fallback': 'tl-title', onclick: () => {
              all = true;
              redraw();
            } }, `Show all ${entries.length}`)
          : null);
    },
  };
}
