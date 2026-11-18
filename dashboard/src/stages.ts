// The stages the fold reports, in the order the board and the list show them.

/** Board order: what needs a person first, finished work last. Mirrors `Stage` in core/src/work_fold.rs. */
export const STAGES = [
  ['needs_you', 'Needs you'],
  ['triage', 'Triage'],
  ['proposed', 'Proposed'],
  ['in_review', 'In review'],
  ['ready', 'Ready'],
  ['blocked', 'Blocked'],
  ['held', 'Working'],
  ['done', 'Done'],
  ['closed', 'Closed'],
] as const;

/** A stage the page does not know yet still shows, under its own name, after the known ones. */
export function stageLabel(stage: string): string {
  const known = STAGES.find(([s]) => s === stage);
  if (known) return known[1];
  const words = stage.replaceAll('_', ' ');
  return words.charAt(0).toUpperCase() + words.slice(1);
}

