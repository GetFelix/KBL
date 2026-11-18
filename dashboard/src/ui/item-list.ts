// The list view: items grouped by stage, one link per row, or the state that
// explains why there are none. A Blocked row also says what it waits on. Rows are links, so Tab, Enter and middle-click work
// natively; j and k move the selection and the focus together.
import { waitsOn } from '../fold.ts';
import { inScope, neighbour, visible, type Group } from '../list.ts';
import { flagText } from '../overlap.ts';
import { toAddress, type AddressState, type ViewState } from '../state.ts';
import { activeCount } from '../views.ts';
import { assigneePerson, needsLine } from '../writes.ts';
import { paintAssignee } from './assignee-chip.ts';
import { h } from './dom.ts';
import { inboxActions, type InboxHandlers } from './inbox-actions.ts';
import { inboxKind, kindLabel } from '../inbox.ts';

export interface ListHandlers extends InboxHandlers {
  open(id: string): void;
  /** Focus landed on a row: it becomes the selection, so keys act on what has focus. */
  select(id: string | null): void;
  /** Toggles the item's mark for a bulk action. */
  mark(id: string): void;
  clearSearch(): void;
  clearFilters(): void;
}

const SKELETON_ROWS = 6;

function emptyState(title: string, body: Node | string, action?: HTMLElement): HTMLElement {
  return h('div', { class: 'empty' }, h('h2', {}, title), h('p', {}, body), action);
}

function code(text: string): HTMLElement {
  return h('code', {}, text);
}

const ADVICE = {
  'sign-in': 'A new sign-in link fixes a rejected or expired token. Reload once you have one.',
  reload: 'The log has records newer than this page reads. Reload to get the current page.',
} as const;

// Built from the store's query, not the address bar, which lags the store by up to 250 ms.
export function itemHref(id: string, s: Omit<AddressState, 'open'>): string {
  return toAddress(location.search, { ...s, open: id }) || '?';
}

/** Says something to a screen reader without moving the focus. */
export type Announce = (text: string) => void;

/** What a redraw needs to keep the keyboard in place: the order and stage of what was drawn. */
export interface Drawn {
  order: string[];
  stage: Map<string, string>;
}

export function drawn(groups: readonly Group[]): Drawn {
  return { order: groups.flatMap((g) => g.items.map((i) => i.id)), stage: new Map(groups.flatMap((g) => g.items.map((i) => [i.id, g.label] as const))) };
}

/**
 * Puts the focus back after a redraw. The focused item keeps it if it is still drawn (and
 * a move to another stage is announced); otherwise the nearest item in the first of
 * `orders` that has one takes it, else #work, so focus never falls to the body. The
 * selection follows, after this render, so the store is not re-entered mid-draw.
 */
export function restoreFocus(
  focusedId: string | undefined,
  before: Drawn,
  now: Drawn,
  nodes: ReadonlyMap<string, HTMLElement>,
  host: HTMLElement,
  quietly: (el: HTMLElement) => void,
  on: { select(id: string | null): void; announce: Announce },
  orders: readonly (readonly string[])[] = [before.order],
) {
  if (!focusedId) return;
  const back = nodes.get(focusedId);
  if (back) {
    quietly(back);
    const was = before.stage.get(focusedId);
    const is = now.stage.get(focusedId);
    if (was && is && was !== is) on.announce(`${focusedId} moved to ${is}.`);
    return;
  }
  const near = orders.map((o) => neighbour(o, focusedId, (id) => nodes.has(id))).find((id) => id !== undefined);
  const target = near ? nodes.get(near) : host.closest<HTMLElement>('main');
  if (target) quietly(target);
  on.announce(`${focusedId} is no longer in this view.`);
  queueMicrotask(() => on.select(near ?? null));
}

// Rows are kept by item id and reused, and the list is only rebuilt when what it shows
// changes. A live record must not take the focus off the row a keyboard user is on.
export function itemList(
  on: ListHandlers,
  board: { el: HTMLElement; render(s: ViewState, columns: Group[]): void; focusSelected(): void },
  announce: Announce,
): { el: HTMLElement; render(s: ViewState): void; focusSelected(): void } {
  const el = h('div', { class: 'list' });
  const inbox = inboxActions(on);
  // A digit presses the numbered button of the row's strip, so a question is answered with one key.
  el.addEventListener('keydown', (e) => {
    if (!/^[1-9]$/.test(e.key) || e.metaKey || e.ctrlKey || e.altKey || e.shiftKey) return;
    const target = e.target as HTMLElement;
    if (target.closest('input, textarea, select, button')) return;
    const press = target.closest('.inbox-item')?.querySelector<HTMLElement>(`.inbox-act [data-key="${e.key}"]`);
    if (!press) return;
    e.preventDefault();
    press.click();
  });
  const rows = new Map<string, HTMLAnchorElement>();
  // One checkbox per row, beside its link (a control cannot sit inside a link), for bulk actions.
  // Out of the Tab order so Tab still moves row to row, as in Linear and GitHub; X marks from the keyboard.
  const marks = new Map<string, HTMLInputElement>();
  const markBox = (id: string): HTMLInputElement => {
    let box = marks.get(id);
    if (!box) {
      box = h('input', { type: 'checkbox', class: 'mark', 'data-mark': id, 'aria-label': `Select ${id}`, tabindex: '-1', onchange: () => on.mark(id) });
      marks.set(id, box);
    }
    return box;
  };
  let shown = '';
  let restoring = false;
  let last: Drawn = drawn([]);
  const quietly = (target: HTMLElement) => {
    restoring = true;
    target.focus({ preventScroll: true });
    restoring = false;
  };
  const focusedHere = () => {
    const active = document.activeElement;
    return active instanceof HTMLElement && el.contains(active) ? (active.dataset.id ?? active.closest<HTMLElement>('[data-id]')?.dataset.id) : undefined;
  };
  el.addEventListener('focusin', (e) => {
    const id = (e.target as HTMLElement).closest<HTMLElement>('.row')?.dataset.id;
    if (id && !restoring) on.select(id);
  });

  const show = (key: string, ...children: HTMLElement[]) => {
    if (key === shown) return;
    const focusedId = focusedHere();
    shown = key;
    rows.clear();
    last = drawn([]);
    el.replaceChildren(...children);
    // A state with no rows: a focused row or card has nowhere to go but #work.
    restoreFocus(focusedId, last, last, rows, el, quietly, { select: on.select, announce }, []);
  };
  const row = (id: string, title: string, waits: string): HTMLAnchorElement => {
    let a = rows.get(id);
    if (!a) {
      a = h('a', {
        class: 'row',
        'data-id': id,
        onclick: (e) => {
          const m = e as MouseEvent;
          if (m.metaKey || m.ctrlKey || m.shiftKey || m.button !== 0) return;
          e.preventDefault();
          on.open(id);
        },
      }, h('span', { class: 'row-id' }, id), h('span', { class: 'row-title' }), h('span', { class: 'row-waits muted' }), h('span', { class: 'row-flag', hidden: true }), h('span', { class: 'assignee', hidden: true }));
      rows.set(id, a);
    }
    const t = a.querySelector('.row-title')!;
    const w = a.querySelector('.row-waits')!;
    if (t.textContent !== title) t.textContent = title;
    if (w.textContent !== waits) w.textContent = waits;
    return a;
  };

  // The board keeps its own cards; the list only decides that it is the board's turn.
  const showBoard = (s: ViewState, columns: Group[]) => {
    if (shown !== 'board') {
      shown = 'board';
      rows.clear();
      last = drawn([]);
      el.replaceChildren(board.el);
    }
    board.render(s, columns);
  };
  const view = {
    el,
    render(s: ViewState) {
      const c = s.connection;
      // After a failed first attempt the retry view stays up until the socket is joined.
      const again = c.kind === 'loading' && !c.linked && (shown.startsWith('retrying') || shown === 'again');
      el.toggleAttribute('aria-busy', !s.synced && s.items.length === 0 && c.kind === 'loading' && !again);
      if (c.kind === 'setup') {
        return show('setup',
          emptyState(
            'Connect to a gateway',
            h('span', {}, 'Sign in with a view link: run ', code('node dev/open-dashboard.mjs'), ' and open the link it prints, or paste a link that carries ', code('?gateway=…&room=…&token=…'), '. This tab remembers it across reloads. The dashboard README shows the scope the gateway needs.'),
          ),
        );
      }
      if (c.kind === 'stopped') {
        // Rebuilt only when the stop changes, so the alert is announced once.
        const advice = c.advice ? ADVICE[c.advice] : null;
        return show(`stopped|${c.reason}|${c.advice}`,
          h('div', { class: 'empty error', role: 'alert' },
            h('h2', {}, 'The dashboard stopped'),
            h('p', {}, c.reason),
            advice ? h('p', {}, advice) : null,
            advice ? h('button', { type: 'button', class: 'button', onclick: () => location.reload() }, 'Reload') : null,
          ),
        );
      }
      if (!s.synced && s.items.length === 0) {
        if (c.kind === 'retrying') {
          return show(`retrying|${c.reason}|${c.seconds}`,
            emptyState('Cannot reach the gateway', `Nothing has been read yet. ${c.reason}. Trying again in ${c.seconds} s.`));
        }
        // A retry after a failed first attempt keeps saying so instead of flashing placeholder rows.
        if (again) {
          return show('again', emptyState('Cannot reach the gateway', 'Nothing has been read yet. Trying again now.'));
        }
        return show('loading',
          h('ul', { class: 'skeleton', 'aria-hidden': 'true' }, ...Array.from({ length: SKELETON_ROWS }, () => h('li', {}, h('span'), h('span')))),
        );
      }
      if (s.items.length === 0) return show('empty', emptyState('No work yet', 'Items appear here as the tracker imports issues from its sources.'));
      if (s.view === 'inbox' && s.kind && inScope(s).length === 0) {
        return show(`nokind|${s.kind}`, emptyState(`No ${kindLabel(s.kind).toLowerCase()} waiting`, 'Pick another kind, or show all of Needs you.'));
      }
      if (s.view === 'inbox' && inScope(s).length === 0) {
        return show('noinbox', emptyState('Nothing needs you', 'An item shows here when an agent asks a question, a check fails, or work runs out of budget or tries.'));
      }
      if (s.view === 'blocked' && inScope(s).length === 0) {
        return show('noblocked', emptyState('Nothing is blocked', 'An item shows here while a link or a prerequisite holds it back.'));
      }
      const groups = visible(s);
      const waits = new Map(s.blocked.map((b) => [b.id, `waits on ${waitsOn(b)}`]));
      for (const i of s.items) {
        if (i.needs) waits.set(i.id, needsLine(i));
        else if (s.view === 'inbox') {
          const k = inboxKind(i);
          if (k) waits.set(i.id, k === 'review' ? 'A review finding to accept' : 'Triage proposed this');
        }
      }
      if (groups.length === 0) {
        const filtered = activeCount(s.filters) > 0;
        const what = s.query ? `“${s.query}”` : 'these filters';
        return show(`nomatch|${s.query}|${JSON.stringify(s.filters)}`,
          emptyState(`Nothing matches ${what}`, filtered ? 'Search looks at item ids and titles, inside the filters.' : 'Search looks at item ids and titles.',
            h('span', { class: 'actions' },
              s.query ? h('button', { type: 'button', class: 'button', onclick: on.clearSearch }, 'Clear search') : null,
              filtered ? h('button', { type: 'button', class: 'button', onclick: on.clearFilters }, 'Clear filters') : null,
            )),
        );
      }
      if (s.layout === 'board') return showBoard(s, groups);
      const key = `list|${JSON.stringify(groups.map((g) => [g.stage, g.items.map((i) => [i.id, i.title, waits.get(i.id) ?? ''])]))}`;
      // A field in a strip loses focus when its list is rebuilt; it gets it back once the strips are in.
      let typing: HTMLElement | null = null;
      if (key !== shown) {
        const focusedId = focusedHere();
        typing = document.activeElement instanceof HTMLElement && document.activeElement.closest('.inbox-act') && el.contains(document.activeElement) ? document.activeElement : null;
        const focusedMark = document.activeElement instanceof HTMLInputElement && marks.get(document.activeElement.dataset.mark ?? '') === document.activeElement ? document.activeElement : null;
        const before = last;
        const kept = new Map(rows);
        shown = key;
        rows.clear();
        for (const g of groups) for (const i of g.items) {
          const a = kept.get(i.id);
          if (a) rows.set(i.id, a);
        }
        el.replaceChildren(
          ...groups.map((g) => {
            const headingId = `group-${g.stage}`;
            return h('section', { class: 'group', 'aria-labelledby': headingId },
              h('h2', { id: headingId, class: 'group-title' }, g.label, h('span', { class: 'count' }, String(g.items.length))),
              h('ul', {}, ...g.items.map((item) => h('li', { class: s.view === 'inbox' ? 'row-item inbox-item' : 'row-item' }, markBox(item.id), row(item.id, item.title, waits.get(item.id) ?? '')))),
            );
          }),
        );
        last = drawn(groups);
        // Moving a focused node blurs it; put the focus back without scrolling or re-selecting.
        restoreFocus(focusedId, before, last, rows, el, quietly, { select: on.select, announce });
        // A checkbox that had the focus keeps it, rather than handing it to its row.
        if (focusedMark?.isConnected) quietly(focusedMark);
        for (const id of marks.keys()) if (!rows.has(id)) marks.delete(id);
      }
      if (s.view === 'inbox') {
        const byItem = new Map(s.items.map((i) => [i.id, i]));
        for (const li of el.querySelectorAll<HTMLElement>('.inbox-item')) {
          const item = byItem.get(li.querySelector<HTMLElement>('.row')?.dataset.id ?? '');
          if (!item) continue;
          const strip = inbox.strip(item, s);
          const old = li.querySelector('.inbox-act');
          if (old !== strip) {
            const had = old?.contains(document.activeElement);
            old ? old.replaceWith(strip) : li.append(strip);
            if (had) strip.querySelector<HTMLElement>('[data-focus]')?.focus({ preventScroll: true });
          }
        }
        inbox.keep(new Set(rows.keys()));
        if (typing?.isConnected && document.activeElement !== typing) quietly(typing);
      }
      const marked = new Set(s.marked);
      for (const [id, box] of marks) if (box.checked !== marked.has(id)) box.checked = marked.has(id);
      const byId = new Map(s.items.map((i) => [i.id, i]));
      for (const [id, a] of rows) {
        const href = itemHref(id, s);
        if (a.getAttribute('href') !== href) a.setAttribute('href', href);
        a.toggleAttribute('data-selected', id === s.selected);
        a.toggleAttribute('data-marked', marked.has(id));
        // Text, not colour alone, so it reads in both themes and to a screen reader as part of the link.
        const flag = a.querySelector<HTMLElement>('.row-flag')!;
        const text = flagText(s.overlaps.get(id));
        if (flag.textContent !== text) flag.textContent = text;
        flag.hidden = text === '';
        paintAssignee(a, assigneePerson(byId.get(id) ?? {}));
        if (id === s.open) a.setAttribute('aria-current', 'true');
        else a.removeAttribute('aria-current');
      }
    },
    focusSelected() {
      if (shown === 'board') return board.focusSelected();
      const selected = el.querySelector<HTMLElement>('[data-selected]');
      selected?.focus();
      selected?.scrollIntoView({ block: 'nearest' });
    },
  };
  return view;
}
