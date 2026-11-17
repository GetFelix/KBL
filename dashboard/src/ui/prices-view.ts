// Models and prices: every model seen in recorded usage with its price per unit, where the price came
// from, what has accrued and how it changed. A signed-in person can set or change a price here; changing
// one in effect shows old to new and waits for a confirm. Only recorded fields are shown.
import { dollars, parseDollars, priceModels, replacedPrice, samePrice, sourceText, unitName, unitPer, type PriceEntry, type PriceModel, type PriceRow, type PriceUnit, type Spend } from '../spend.ts';
import { modal } from './dialog.ts';
import { h } from './dom.ts';

export interface PricesHandlers {
  /** Whether the viewer is signed in to the tracker, which is what setting a price needs. */
  canSetPrice(): boolean;
  /** Sends a price typed in dollars, from an optional UTC day; resolves to why it was refused, or null once written. */
  /** `replaced` is the price the person saw and confirmed; the tracker refuses if another is in effect by then. */
  setPrice(model: string, unit: PriceUnit, dollarsText: string, from: string, replaced: PriceEntry | null): Promise<string | null>;
  now(): number;
}

const COUNT = new Intl.NumberFormat('en-US', { maximumFractionDigits: 3 });
const day = (ms: number) => new Date(ms).toISOString().slice(0, 10);

function priceCell(r: PriceRow): Node {
  if (!r.current) return h('span', { class: r.waiting ? 'unpriced' : 'muted' }, r.waiting ? 'waiting for a price' : 'no price');
  return document.createTextNode(`${dollars(r.current.price_micros)} per ${unitPer(r.unit)}`);
}

function sinceCell(r: PriceRow): Node {
  const next = r.scheduled.map((p) => `then ${dollars(p.price_micros)} from ${day(p.effective_from_ms)}`).join('; ');
  return h('span', {}, r.current ? day(r.current.effective_from_ms) : '', next ? h('span', { class: 'muted small' }, `${r.current ? ' ' : ''}${next}`) : null);
}

function historyList(list: PriceEntry[]): Node {
  if (!list.length) return h('p', { class: 'muted small' }, 'No price has been set for this model.');
  return h('table', { class: 'spend-table' },
    h('thead', {}, h('tr', {}, ...['Starts', 'Unit', 'Source'].map((c) => h('th', { scope: 'col' }, c)), h('th', { scope: 'col', class: 'num' }, 'Price'))),
    h('tbody', {}, ...list.map((p) => h('tr', {}, h('td', {}, day(p.effective_from_ms)), h('td', {}, unitName(p.unit)), h('td', {}, sourceText(p)), h('td', { class: 'num' }, `${dollars(p.price_micros)} per ${unitPer(p.unit)}`)))),
  );
}

export function pricesView(on: PricesHandlers) {
  const m = modal('Models and prices', 'prices');
  const close = h('button', { type: 'button', class: 'button ghost', onclick: () => m.close() }, 'Close');
  // The row being edited survives the page refreshing when the fold moves.
  let editing: { model: string; unit: PriceUnit } | null = null;
  let lastSpend: Spend | null = null;
  // Shown under the page, so it survives the redraw that follows a save.
  let statusText = '';

  // render drops the focused button, so focus goes back to the row's own.
  const focusRow = (model: string, unit: PriceUnit) => {
    ((Array.from(m.body.querySelectorAll('button[data-price-row]')) as HTMLElement[]).find((b) => b.dataset.priceRow === `${model}|${unit}`))?.focus();
  };

  const render = (spend: Spend) => {
    lastSpend = spend;
    const now = on.now();
    const can = on.canSetPrice();
    const status = h('p', { class: 'muted small', role: 'status' }, statusText);

    const form = (pm: PriceModel, r: PriceRow): Node => {
      const price = h('input', { type: 'text', inputmode: 'decimal', size: '9', id: 'price-input', 'aria-label': `Dollars per ${unitPer(r.unit)} for ${pm.model}` }) as HTMLInputElement;
      const from = h('input', { type: 'text', size: '10', placeholder: 'YYYY-MM-DD', 'aria-label': 'Starts on (UTC day, optional; blank means now)' }) as HTMLInputElement;
      const confirm = h('input', { type: 'checkbox', id: 'price-confirm' }) as HTMLInputElement;
      const change = h('p', { class: 'small', role: 'status' });
      const save = h('button', { type: 'button', class: 'button', disabled: true }) as HTMLButtonElement;
      save.textContent = r.current ? 'Change price' : 'Set price';
      // The price a change replaces is the one in effect when it starts, read from the newest fold. The
      // confirm covers `asked`: if the fold moves it before Save, the person is asked again.
      const replaced = () => replacedPrice(lastSpend ?? spend, pm.model, r.unit, from.value, on.now());
      let asked = replaced();
      const update = () => {
        const micros = parseDollars(price.value);
        const old = replaced();
        if (!samePrice(old, asked)) {
          asked = old;
          confirm.checked = false;
        }
        change.textContent = typeof micros === 'number' && asked ? `${dollars(asked.price_micros)} → ${dollars(micros)} per ${unitPer(r.unit)}` : '';
        save.disabled = typeof micros !== 'number' || (asked !== null && !confirm.checked);
      };
      price.addEventListener('input', update);
      from.addEventListener('input', update);
      confirm.addEventListener('change', update);
      save.addEventListener('click', async () => {
        const now = replaced();
        if (!samePrice(now, asked)) {
          // Someone changed it after the form was drawn: show the new change and ask again.
          asked = now;
          confirm.checked = false;
          update();
          statusText = now ? `The price changed to ${dollars(now.price_micros)} while this was open. Check it and confirm again.` : 'The price was removed while this was open. Check it and try again.';
          status.textContent = statusText;
          confirm.focus();
          return;
        }
        save.disabled = true;
        const why = await on.setPrice(pm.model, r.unit, price.value, from.value, asked);
        statusText = why ?? `Price set for ${pm.model}, ${unitName(r.unit).toLowerCase()}.`;
        status.textContent = statusText;
        if (why === null) {
          editing = null;
          if (lastSpend) render(lastSpend);
          focusRow(pm.model, r.unit);
        } else {
          // A refused change is confirmed again: the price it replaces may have moved.
          confirm.checked = false;
          update();
        }
      });
      return h('form', { class: 'price-edit', 'aria-label': `${r.current ? 'Change' : 'Set'} ${unitName(r.unit).toLowerCase()} price for ${pm.model}`, onsubmit: (e) => e.preventDefault() },
        h('p', { class: 'small' }, `${unitName(r.unit)} price for ${pm.model}`, r.current ? ` (now ${dollars(r.current.price_micros)} per ${unitPer(r.unit)}, ${sourceText(r.current)})` : ''),
        h('p', { class: 'price-form' }, price, from),
        change,
        r.current
          ? h('p', { class: 'small' }, confirm, h('label', { for: 'price-confirm' }, ' I am changing a price that is in effect. Usage already charged keeps its price.'))
          : null,
        h('p', {}, save, ' ', h('button', { type: 'button', class: 'button ghost', onclick: () => { editing = null; statusText = ''; render(lastSpend ?? spend); focusRow(pm.model, r.unit); } }, 'Cancel')),
      );
    };

    const section = (pm: PriceModel): Node => {
      const open = editing?.model === pm.model ? pm.rows.find((r) => r.unit === editing?.unit) : undefined;
      return h('section', { class: 'price-model-section', 'data-model': pm.model, 'aria-labelledby': `pm-${pm.model}` },
        h('h3', { id: `pm-${pm.model}`, class: 'price-model' }, pm.model),
        h('table', { class: 'spend-table', 'aria-labelledby': `pm-${pm.model}` },
          h('thead', {}, h('tr', {}, ...['Unit', 'Price', 'Since', 'Source', 'Usage'].map((c) => h('th', { scope: 'col' }, c)), h('th', { scope: 'col' }, h('span', { class: 'sr-only' }, 'Action')))),
          h('tbody', {}, ...pm.rows.map((r) =>
            h('tr', { 'data-unit': r.unit },
              h('th', { scope: 'row' }, unitName(r.unit)),
              h('td', {}, priceCell(r)),
              h('td', {}, sinceCell(r)),
              h('td', {}, r.current ? sourceText(r.current) : ''),
              h('td', { class: 'usage' }, r.usage > 0 ? COUNT.format(r.usage) : 'none'),
              h('td', { class: 'num' }, can
                ? h('button', { type: 'button', class: 'button ghost', 'aria-label': `${r.current ? 'Change' : 'Set'} ${unitName(r.unit).toLowerCase()} price for ${pm.model}`, 'data-price-row': `${pm.model}|${r.unit}`, onclick: () => { editing = { model: pm.model, unit: r.unit }; statusText = ''; render(lastSpend ?? spend); (m.body.querySelector('#price-input') as HTMLInputElement | null)?.focus(); } }, r.current ? 'Change' : 'Set')
                : null)))),
        ),
        open ? form(pm, open) : null,
        h('details', {}, h('summary', {}, `History (${pm.history.length})`), historyList(pm.history)),
      );
    };

    const list = priceModels(spend, now);
    // A live record redraws the page after a save; the row's button keeps focus across it.
    const had = (document.activeElement as HTMLElement | null)?.dataset?.priceRow;
    m.body.replaceChildren(
      h('header', { class: 'dialog-head' }, h('h2', {}, 'Models and prices'), close),
      h('p', { class: 'muted small spend-note' }, 'Every model seen in recorded usage, priced or not. A price starts when it is set, or on the day you choose, and never changes what was already charged.'),
      ...(can ? [] : [h('p', { class: 'muted small' }, 'Sign in to the tracker to set or change a price.')]),
      ...(list.length === 0 ? [h('p', { class: 'muted' }, 'No model has reported usage yet.')] : list.map(section)),
      status,
    );
    if (had) focusRow(...(had.split('|') as [string, PriceUnit]));
  };

  return {
    el: m.el,
    isOpen: () => m.isOpen(),
    /** Redraws an open page for a fold that moved, leaving it alone while a price is being typed. */
    refresh(spend: Spend) {
      lastSpend = spend;
      if (m.isOpen() && editing === null) render(spend);
    },
    /** Fills the page and opens it; null while the history is still being read. */
    open(spend: Spend | null) {
      const wasOpen = m.isOpen();
      if (!spend) {
        m.body.replaceChildren(
          h('header', { class: 'dialog-head' }, h('h2', {}, 'Models and prices'), close),
          h('p', { class: 'muted', role: 'status' }, 'Still reading the history. Prices appear when it is loaded.'),
        );
      } else {
        render(spend);
      }
      if (wasOpen) return;
      m.open();
      close.focus();
    },
  };
}
