// The panel field for a finished attempt's handoff. Left out until some attempt has finished.
import { handoffKey } from '../handoff.ts';
import type { ItemRow } from '../fold.ts';
import type { ViewState } from '../state.ts';
import { canRate } from '../writes.ts';
import { handoffBlock } from './handoff-block.ts';
import type { Field, PanelHandlers } from './item-panel.ts';

const enabled = (item: ItemRow, s: ViewState): boolean => s.tracker.kind === 'signed-in' && canRate(item);

export const handoffField: Field = {
  key: (item, s) => `${handoffKey(item, s.handoffs?.get(item.id))}|${enabled(item, s)}`,
  render: (item, s, on: PanelHandlers) => {
    const ho = s.handoffs?.get(item.id);
    return ho ? ['Handoff', handoffBlock(item, ho, { enabled: enabled(item, s), mark: (attempt, accurate) => on.rate(item.id, attempt, accurate) }, 'panel')] : null;
  },
};
