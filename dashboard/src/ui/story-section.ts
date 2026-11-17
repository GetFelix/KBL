// The panel's "Story" section: the decisions the working agent recorded, grouped by
// phase, with the edits and evidence behind each and the flags where a claim and the
// recorded steps disagree. Every line is a recorded field; offsets let a reader find
// the record. See docs/decision-replay.md.
import { PHASE_LABEL, attemptFlags, citedSteps, citedText, contentText, decisionCount, flagsFor, type CitedStep, type DecisionLine, type EditLine, type Flag, type Story } from '../story.ts';
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

/** Where a reference can land: the offsets of the records the section renders. */
interface Targets {
  has: (offset: number) => boolean;
  go: (offset: number) => void;
}

// The focus key holds numbers only: the label can carry recorded text, and a key is used in a selector.
function offsetButton(label: string, offset: number, scope: string, t: Targets): Node | string {
  if (!t.has(offset)) return label;
  return h('button', { type: 'button', class: 'story-ref', 'data-focus': `ref:${scope}:${offset}`, onclick: () => t.go(offset) }, label);
}

function stepLine(c: CitedStep): Node {
  return h('li', { class: 'story-step', 'data-at': String(c.offset), tabindex: '-1' },
    h('code', {}, c.command),
    h('span', { class: 'muted' }, ` exit ${c.exit_code}, step ${c.offset}`));
}

function flagList(flags: Flag[], scope: string, t: Targets): Node | null {
  if (flags.length === 0) return null;
  return h('ul', { class: 'story-flags' },
    ...flags.map((f) => h('li', { class: 'story-flag', 'data-rule': f.rule },
      h('span', { class: 'story-flag-text' }, f.message),
      f.decision !== null ? h('span', {}, ' ', offsetButton(`decision ${f.decision}`, f.decision, scope, t)) : `. Attempt ${f.attempt}`,
      ...f.offsets.map((o) => h('span', {}, ' ', offsetButton(`offset ${o}`, o, scope, t))))));
}

function editLine(e: EditLine, decision: number, t: Targets): Node {
  const hunk = contentText(e.hunk);
  return h('li', { class: 'story-edit', 'data-at': String(e.offset), tabindex: '-1' },
    h('details', {},
      h('summary', {}, h('code', {}, e.file), ` (edit ${e.offset})`),
      hunk === null ? h('p', { class: 'muted' }, 'The hunk is stored as an artifact.') : h('pre', { class: 'story-hunk' }, hunk)),
    // Outside the summary: a control inside a summary is nested interactive content.
    h('p', {}, 'Carries out ', offsetButton(`decision ${decision}`, decision, `edit${e.offset}`, t)));
}

function decisionLine(d: DecisionLine, story: Story, t: Targets): Node {
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
          c.found ? offsetButton(citedText(c), c.offset, `d${d.offset}`, t) : citedText(c))))
      : null,
    d.edits.length > 0 ? h('ul', { class: 'story-edits' }, ...d.edits.map((e) => editLine(e, d.offset, t))) : null,
    flagList(flagsFor(story, d.offset), `d${d.offset}`, t));
}

function storySection(story: Story): Node {
  let root: HTMLElement | null = null;
  const steps = citedSteps(story);
  const known = new Set<number>([
    ...steps.map((c) => c.offset),
    ...story.people.map((p) => p.offset),
    ...story.groups.flatMap((g) => g.decisions.flatMap((d) => [d.offset, ...d.edits.map((e) => e.offset)])),
  ]);
  const t: Targets = { has: (o) => known.has(o), go: (o) => jump(() => root, o) };
  const loose = attemptFlags(story);
  root = h('div', { class: 'story' },
    h('p', { class: 'muted' }, `${decisionCount(story)} recorded ${decisionCount(story) === 1 ? 'decision' : 'decisions'} across ${story.attempts.length} ${story.attempts.length === 1 ? 'attempt' : 'attempts'}. Flags come from fixed rules over what was recorded; a step nobody recorded is invisible to them.`),
    flagList(loose, 'story', t),
    ...story.groups.map((g) => h('section', { class: 'story-phase', 'data-phase': g.phase },
      h('h3', {}, PHASE_LABEL[g.phase]),
      h('ol', { class: 'story-decisions' }, ...g.decisions.map((d) => decisionLine(d, story, t))))),
    steps.length > 0
      ? h('section', { class: 'story-steps' },
          h('h3', {}, 'Cited steps'),
          h('ul', {}, ...steps.map(stepLine)))
      : null,
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
    return story ? String(story.rev) : '';
  },
  render: (item, s) => {
    const story = s.stories.get(item.id);
    return story ? ['Story', storySection(story)] : null;
  },
};
