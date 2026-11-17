// The panel's "Story" section: the decisions the working agent recorded, grouped by
// phase, with the edits and evidence behind each and the flags where a claim and the
// recorded steps disagree. Every line is a recorded field; offsets let a reader find
// the record. See docs/decision-replay.md.
import { PHASE_LABEL, attemptFlags, citedText, contentText, decisionCount, flagsFor, type DecisionLine, type EditLine, type Flag, type Story } from '../story.ts';
import { h } from './dom.ts';
import type { Field } from './item-panel.ts';

/** Moves to the record with this offset in the story, opening what holds it. */
function jump(root: () => HTMLElement | null, offset: number) {
  const target = root()?.querySelector<HTMLElement>(`[data-at="${offset}"]`);
  if (!target) return;
  for (let p: HTMLElement | null = target; p; p = p.parentElement) if (p instanceof HTMLDetailsElement) p.open = true;
  target.scrollIntoView({ block: 'nearest' });
  target.focus({ preventScroll: true });
}

function offsetButton(label: string, offset: number, go: (offset: number) => void) {
  return h('button', { type: 'button', class: 'story-ref', 'data-focus': `ref:${label}:${offset}`, onclick: () => go(offset) }, label);
}

function flagList(flags: Flag[], go: (offset: number) => void): Node | null {
  if (flags.length === 0) return null;
  return h('ul', { class: 'story-flags' },
    ...flags.map((f) => h('li', { class: 'story-flag', 'data-rule': f.rule },
      h('span', { class: 'story-flag-text' }, f.message),
      f.decision !== null ? h('span', {}, ' ', offsetButton(`decision ${f.decision}`, f.decision, go)) : `. Attempt ${f.attempt}`,
      ...f.offsets.map((o) => h('span', {}, ' ', offsetButton(`offset ${o}`, o, go))))));
}

function editLine(e: EditLine, decision: number, go: (offset: number) => void): Node {
  const hunk = contentText(e.hunk);
  return h('li', { class: 'story-edit', 'data-at': String(e.offset), tabindex: '-1' },
    h('details', {},
      h('summary', {}, h('code', {}, e.file), ` (edit ${e.offset}) carries out `, offsetButton(`decision ${decision}`, decision, go)),
      hunk === null ? h('p', { class: 'muted' }, 'The hunk is stored as an artifact.') : h('pre', { class: 'story-hunk' }, hunk)));
}

function decisionLine(d: DecisionLine, story: Story, go: (offset: number) => void): Node {
  const rc = d.revert_check;
  return h('li', { class: 'story-decision', 'data-at': String(d.offset), tabindex: '-1' },
    h('p', { class: 'story-chosen' }, h('strong', {}, d.chosen), h('span', { class: 'muted' }, ` decision ${d.offset}, attempt ${d.attempt}, ${d.by}`)),
    h('p', { class: 'muted' }, d.question),
    h('p', {}, h('span', { class: 'story-label' }, 'Why '), d.why),
    h('p', {}, h('span', { class: 'story-label' }, 'So far '), d.so_far),
    d.options.length > 0 ? h('p', { class: 'muted' }, `Considered: ${d.options.join(', ')}`) : null,
    rc ? h('p', {}, h('span', { class: 'story-label' }, 'Revert check '), `${rc.test}: ${d.proven ? 'failed without the fix, passed with it' : 'did not prove it'}`) : null,
    d.evidence.length > 0
      ? h('ul', { class: 'story-evidence' }, ...d.evidence.map((c) => h('li', {},
          c.found ? offsetButton(citedText(c), c.offset, go) : citedText(c))))
      : null,
    d.edits.length > 0 ? h('ul', { class: 'story-edits' }, ...d.edits.map((e) => editLine(e, d.offset, go))) : null,
    flagList(flagsFor(story, d.offset), go));
}

function storySection(story: Story): Node {
  let root: HTMLElement | null = null;
  const go = (offset: number) => jump(() => root, offset);
  const loose = attemptFlags(story);
  root = h('div', { class: 'story' },
    h('p', { class: 'muted' }, `${decisionCount(story)} recorded ${decisionCount(story) === 1 ? 'decision' : 'decisions'} across ${story.attempts.length} ${story.attempts.length === 1 ? 'attempt' : 'attempts'}. Flags come from fixed rules over what was recorded; a step nobody recorded is invisible to them.`),
    flagList(loose, go),
    ...story.groups.map((g) => h('section', { class: 'story-phase', 'data-phase': g.phase },
      h('h3', {}, PHASE_LABEL[g.phase]),
      h('ol', { class: 'story-decisions' }, ...g.decisions.map((d) => decisionLine(d, story, go))))),
    story.people.length > 0
      ? h('section', { class: 'story-people' },
          h('h3', {}, 'People'),
          h('ul', {}, ...story.people.map((p) => h('li', { 'data-at': String(p.offset), tabindex: '-1' },
            h('span', { class: 'muted' }, `${p.by} (${p.input.replace('_', ' ')}${p.attempt === null ? '' : `, attempt ${p.attempt}`}, record ${p.offset}) `),
            contentText(p.text) ?? 'Stored as an artifact.'))))
      : null);
  return root;
}

/** The panel field for the story. It is left out for an item with no recorded decision. */
export const storyField: Field = {
  key: (item, s) => {
    const story = s.stories.get(item.id);
    return story ? JSON.stringify(story) : '';
  },
  render: (item, s) => {
    const story = s.stories.get(item.id);
    return story ? ['Story', storySection(story)] : null;
  },
};
