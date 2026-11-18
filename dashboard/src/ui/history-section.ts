// The panel's History: one row per attempt with how it ended, what it handed off, what it cost
// and whether a person marked the summary accurate, with a way into the decision replay.
import { history, money, type ActivityEvent, type AttemptHistory } from '../activity.ts';
import type { ItemRow } from '../fold.ts';
import type { Story } from '../story.ts';
import { h } from './dom.ts';
import { timeEl } from './timeline-section.ts';

/** Moves to the first recorded decision of `attempt` in the Story section, opening what holds it. */
function replay(root: HTMLElement, attempt: number): void {
  const target = root.querySelector<HTMLElement>(`.story-decision[data-attempt="${attempt}"]`);
  if (!target) return;
  for (let p: HTMLElement | null = target; p; p = p.parentElement) if (p instanceof HTMLDetailsElement) p.open = true;
  target.scrollIntoView({ block: 'nearest' });
  target.focus({ preventScroll: true });
}

function mark(a: AttemptHistory): string {
  if (a.accurate === null) return a.outcome === 'Finished' ? 'Summary not marked' : '';
  return a.accurate ? 'Summary marked accurate' : 'Summary marked not accurate';
}

export function historySection(item: ItemRow, events: readonly ActivityEvent[], story: Story | undefined, root: () => HTMLElement): HTMLElement | null {
  const rows = history(events, item);
  if (rows.length === 0) return null;
  const decided = new Set(story?.groups.flatMap((g) => g.decisions.map((d) => d.attempt)) ?? []);
  const total = rows.reduce((n, a) => n + a.cost_micros, 0);
  return h('section', { class: 'history', 'aria-labelledby': 'history-title' },
    h('h3', { id: 'history-title', class: 'section-title' }, 'Attempts'),
    h('ol', { class: 'attempts' },
      ...rows.map((a) => h('li', { class: 'attempt', 'data-attempt': String(a.attempt) },
        h('p', {}, h('strong', {}, `Attempt ${a.attempt}`), h('span', { class: 'muted' }, ` ${a.by}, started `), timeEl(a.started_ms)),
        h('p', {}, a.outcome),
        a.summary ? h('p', { class: 'attempt-summary' }, h('span', { class: 'story-label' }, 'Summary '), a.summary) : null,
        h('p', { class: 'muted' }, [mark(a), `${a.steps} ${a.steps === 1 ? 'step' : 'steps'}${a.failedSteps ? `, ${a.failedSteps} failed` : ''}`, `cost ${money(a.cost_micros)}`].filter(Boolean).join('. ')),
        decided.has(a.attempt)
          ? h('button', { type: 'button', class: 'button', 'data-focus': `replay:${a.attempt}`, 'aria-label': `Decision replay of attempt ${a.attempt}`, onclick: () => replay(root(), a.attempt) }, 'Decision replay')
          : null))),
    rows.length > 1 ? h('p', { class: 'muted' }, `Total cost ${money(total)}`) : null);
}
