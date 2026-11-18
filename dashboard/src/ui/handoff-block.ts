// The trust block beside a finished attempt: what it said, what it changed, whether the
// pull request's checks pass, which steps failed, and one action to mark the summary. The
// panel and the Needs-you inbox draw the same block. Everything shown is a recorded field.
import { checksText, diffText, failedText, markText, marks, prUrl, type Handoff } from '../handoff.ts';
import type { ItemRow } from '../fold.ts';
import { h } from './dom.ts';

export interface HandoffMarker {
  /** Whether the viewer can write: signed in, on a log that takes the mark. */
  enabled: boolean;
  /** Marks `attempt` of `item`. The attempt is the one the block was built for, never read later. */
  mark(attempt: number, accurate: boolean): void;
}

/**
 * `scope` keeps ids and focus keys unique where the block appears twice (the panel and an inbox
 * row can both be on screen). Buttons are plain, so Tab reaches them and Enter or Space sends.
 */
export function handoffBlock(item: ItemRow, ho: Handoff, marker: HandoffMarker, scope: string): HTMLElement {
  const id = `handoff-${scope}-${item.id}`;
  const url = prUrl(item.pr);
  const checks = checksText(item);
  const failed = failedText(ho);
  return h('div', { class: 'handoff', 'data-attempt': String(ho.attempt), role: 'group', 'aria-labelledby': `${id}-title` },
    h('p', { class: 'handoff-title', id: `${id}-title` }, h('strong', {}, `Attempt ${ho.attempt} handed off`)),
    ho.summary ? h('p', { class: 'handoff-summary' }, h('span', { class: 'story-label' }, 'Summary '), ho.summary) : h('p', { class: 'muted' }, 'No summary note recorded.'),
    h('ul', { class: 'handoff-facts' },
      h('li', {}, h('span', { class: 'story-label' }, 'Diff '), diffText(ho)),
      item.pr ? h('li', {}, h('span', { class: 'story-label' }, 'Item’s pull request '), url ? h('a', { href: url, target: '_blank', rel: 'noopener noreferrer' }, item.pr) : item.pr) : null,
      checks ? h('li', { class: 'handoff-checks', 'data-checks': item.pr_checks ?? '' }, h('span', { class: 'story-label' }, 'Item’s checks '), checks.replace(/^Checks /, '')) : null,
      failed ? h('li', {}, h('span', { class: 'story-label' }, 'Steps '), failed) : null),
    ho.failed_steps.length > 0
      ? h('ul', { class: 'handoff-failed' }, ...ho.failed_steps.map((f) => h('li', {}, h('code', {}, f.command), h('span', { class: 'muted' }, ` exit ${f.exit_code}, step ${f.offset}`))))
      : null,
    h('p', { class: 'handoff-mark' },
      h('span', { class: 'muted', id: `${id}-mark` }, `${markText(ho)}. `),
      ...(marker.enabled
        ? marks(ho).map((m) => h('button', {
            type: 'button',
            class: 'button small',
            'data-focus': `handoff-${scope}:${m.accurate ? 'yes' : 'no'}`,
            'aria-pressed': String(m.pressed),
            'aria-describedby': `${id}-mark`,
            onclick: () => marker.mark(ho.attempt, m.accurate),
          }, m.label))
        : [])));
}
