// The small initials chip on a row or card. The name is in its label, so it does not rest on colour or letters alone.
import { initials } from '../writes.ts';

/** Shows `who` on the chip inside `el`, or hides the chip when nobody is assigned. */
export function paintAssignee(el: HTMLElement, who: string | null): void {
  const chip = el.querySelector<HTMLElement>('.assignee');
  if (!chip) return;
  const text = who === null ? '' : initials(who);
  if (chip.textContent !== text) chip.textContent = text;
  const label = who === null ? '' : `Assigned to ${who}`;
  if (chip.getAttribute('aria-label') !== label) {
    if (label) chip.setAttribute('aria-label', label);
    else chip.removeAttribute('aria-label');
    chip.title = label;
  }
  chip.hidden = who === null;
}
