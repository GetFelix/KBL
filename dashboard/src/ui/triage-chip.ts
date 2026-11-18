// The small chip on a row or card that says where a root's triage child stands.
import type { ItemRow } from '../fold.ts';
import { triageChip } from '../list.ts';

/** Shows the triage chip inside `el`, or hides it when the item is not a root with a triage child. */
export function paintTriage(el: HTMLElement, item: ItemRow | undefined): void {
  const chip = el.querySelector<HTMLElement>('.triage-chip');
  if (!chip) return;
  const text = item ? triageChip(item) ?? '' : '';
  if (chip.textContent !== text) chip.textContent = text;
  chip.hidden = text === '';
}
