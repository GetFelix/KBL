// The list view: items grouped by stage, one link per row, or the state that
// explains why there are none. A Blocked row also says what it waits on. Rows are links, so Tab, Enter and middle-click work
// natively; j and k move the selection and the focus together.
import { childIndex, lead, nameOf, properties, reasonLine, summarize, workingLine } from '../card.ts';
import { waitsOn } from '../fold.ts';
import { inScope, neighbour, visible, type Group } from '../list.ts';
import { flagText } from '../overlap.ts';
import { toAddress, type AddressState, type ViewState } from '../state.ts';
import { activeCount } from '../views.ts';
import { assigneePerson } from '../writes.ts';
import { paintAssignee } from './assignee-chip.ts';
import { childStrip, focusedStrip, refocusChip, stripKey, type StripHandlers } from './child-strip.ts';
import { paintTriage } from './triage-chip.ts';
import { byFocus, h } from './dom.ts';
import { inboxActions, type InboxHandlers } from './inbox-actions.ts';
import { inboxKind, kindLabel } from '../inbox.ts';

export interface ListHandlers extends StripHandlers, InboxHandlers {
  open(id: string): void;
  /** Focus landed on a row: it becomes the selection, so keys act on what has focus. */
  select(id: string | null): void;
  /** Toggles the item's mark for a bulk action. */
  mark(id: string): void;
  clearSearch(): void;
  clearFilters(): void;
  /** Folds a group away or opens it, by its `collapseKey`. */
  toggleGroup(key: string): void;
}

const SKELETON_ROWS = 6;

function emptyState(title: string, body: Node | string, action?: HTMLElement): HTMLElement {
  return h('div', { class: 'empty' }, h('h2', {}, title), h('p', {}, body), action);
}

function code(text: string): HTMLElement {
  return h('code', {}, text);
}

/** What a person reads for a stop code; the code and the gateway's words stay in a details line. */
const STOP_TEXT: Record<string, string> = {
  signed_out: 'Your session ended.',
  forbidden: 'This link no longer gives access.',
  newer_format: 'This page is older than the log.',
};

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
  const drawnGroups = groups.filter((g) => !g.collapsed);
  return { order: drawnGroups.flatMap((g) => g.items.map((i) => i.id)), stage: new Map(drawnGroups.flatMap((g) => g.items.map((i) => [i.id, g.label] as const))) };
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
  on: { select(id: string | null): void; announce: Announce; name?(id: string): string },
  orders: readonly (readonly string[])[] = [before.order],
) {
  if (!focusedId) return;
  const name = on.name?.(focusedId) ?? focusedId;
  const back = nodes.get(focusedId);
  if (back) {
    quietly(back);
    const was = before.stage.get(focusedId);
    const is = now.stage.get(focusedId);
    if (was && is && was !== is) on.announce(`${name} moved to ${is}.`);
    return;
  }
  const near = orders.map((o) => neighbour(o, focusedId, (id) => nodes.has(id))).find((id) => id !== undefined);
  const target = near ? nodes.get(near) : host.closest<HTMLElement>('main');
  if (target) quietly(target);
  on.announce(`${name} is no longer in this view.`);
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
    if (target.closest('input, textarea, select')) return;
    if (target.closest('button') && !target.closest('.inbox-act')) return;
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
      }, h('span', { class: 'row-id row-lead' }, id), h('span', { class: 'row-title' }), h('span', { class: 'row-waits muted' }), h('span', { class: 'row-props muted' }), h('span', { class: 'row-flag', hidden: true }), h('span', { class: 'triage-chip', hidden: true }), h('span', { class: 'assignee', hidden: true }));
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
        const plain = c.code ? STOP_TEXT[c.code] : undefined;
        return show(`stopped|${c.reason}|${c.advice}`,
          h('div', { class: 'empty error', role: 'alert' },
            h('h2', {}, 'The dashboard stopped'),
            h('p', {}, plain ?? c.reason),
            plain ? h('p', { class: 'muted small', 'data-stop-detail': '' }, c.reason) : null,
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
      const now = Date.now();
      const kids = childIndex(s.items);
      const open = new Set(s.expanded ?? []);
      const strips = (id: string) => stripKey(kids.has(id) ? summarize(kids.get(id)!) : undefined, open.has(id));
      const blocked = new Map(s.blocked.map((b) => [b.id, waitsOn(b)]));
      // What the row says on its right: why it needs a person, else the agent working it, else why it is not moving.
      const waits = new Map(s.items.map((i) => [i.id, (i.needs ? null : workingLine(i, now)) ?? reasonLine(i, blocked.get(i.id))] as const));
      if (s.view === 'inbox') {
        for (const i of s.items) {
          if (i.needs) continue;
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
      // The inbox is one ordered list; the board has no column for it.
      if (s.layout === 'board' && s.view !== 'inbox') return showBoard(s, groups);
      const key = `list|${JSON.stringify(groups.map((g) => [g.stage, g.label, !!g.collapsed, g.collapsed ? g.items.length : g.items.map((i) => [i.id, i.title, waits.get(i.id) ?? '', lead(i), strips(i.id)])]))}`;
      // A field in a strip loses focus when its list is rebuilt; it gets it back once the strips are in.
      let typing: HTMLElement | null = null;
      if (key !== shown) {
        const focusedId = focusedHere();
        const focusedChip = focusedStrip(el);
        typing = document.activeElement instanceof HTMLElement && document.activeElement.closest('.inbox-act') && el.contains(document.activeElement) ? document.activeElement : null;
        const focusedMark = document.activeElement instanceof HTMLInputElement && marks.get(document.activeElement.dataset.mark ?? '') === document.activeElement ? document.activeElement : null;
        const before = last;
        const kept = new Map(rows);
        shown = key;
        rows.clear();
        // Which header had the focus, so folding or regrouping does not drop it to the page.
        const focusedHeader = document.activeElement instanceof HTMLElement && el.contains(document.activeElement) ? document.activeElement.dataset.toggle : undefined;
        for (const g of groups) if (!g.collapsed) for (const i of g.items) {
          const a = kept.get(i.id);
          if (a) rows.set(i.id, a);
        }
        el.replaceChildren(
          ...groups.map((g) => {
            // Escaped, not flattened, so two values that differ only in punctuation keep distinct ids.
            const headingId = `group-${g.stage.replace(/[^\w-]/g, (c) => `_${c.charCodeAt(0).toString(16)}`)}`;
            const count = h('span', { class: 'count' }, String(g.items.length));
            // A grouped list folds: the header is a button, so Tab, Enter and Space already work.
            const title = g.toggle
              ? h('h2', { id: headingId, class: 'group-title' },
                  h('button', { type: 'button', class: 'group-toggle', 'data-toggle': g.toggle, 'aria-expanded': String(!g.collapsed), 'aria-controls': g.collapsed ? undefined : `${headingId}-rows`, onclick: () => on.toggleGroup(g.toggle!) },
                    h('span', { class: 'chevron', 'aria-hidden': 'true' }, g.collapsed ? '▸' : '▾'), h('span', { class: 'group-name' }, g.label), count))
              : g.label ? h('h2', { id: headingId, class: 'group-title' }, g.label, count) : null;
            return h('section', { class: 'group', 'aria-labelledby': g.label ? headingId : undefined, 'aria-label': g.label ? undefined : 'Work' },
              title,
              g.collapsed ? null : h('ul', { id: `${headingId}-rows` }, ...g.items.map((item) => h('li', { class: s.view === 'inbox' ? 'row-item inbox-item' : 'row-item' }, markBox(item.id), row(item.id, item.title, waits.get(item.id) ?? ''), kids.has(item.id) && !s.childCards && s.view !== 'inbox' ? childStrip(item.id, summarize(kids.get(item.id)!), open.has(item.id), (id) => itemHref(id, s), on) : null))),
            );
          }),
        );
        last = drawn(groups);
        if (focusedHeader) Array.from(el.querySelectorAll<HTMLElement>('[data-toggle]')).find((b) => b.dataset.toggle === focusedHeader)?.focus({ preventScroll: true });
        // Moving a focused node blurs it; put the focus back without scrolling or re-selecting.
        restoreFocus(focusedId, before, last, rows, el, quietly, { select: on.select, announce, name: (id) => nameOf(s.items, id) });
        // A checkbox that had the focus keeps it, rather than handing it to its row.
        if (focusedMark?.isConnected) quietly(focusedMark);
        refocusChip(focusedChip, el);
        for (const id of marks.keys()) if (!rows.has(id)) marks.delete(id);
      }
      if (s.view === 'inbox') {
        const byItem = new Map(s.items.map((i) => [i.id, i]));
        for (const li of Array.from(el.querySelectorAll<HTMLElement>('.inbox-item'))) {
          const item = byItem.get(li.querySelector<HTMLElement>('.row')?.dataset.id ?? '');
          if (!item) continue;
          const strip = inbox.strip(item, s);
          const old = li.querySelector('.inbox-act');
          if (old !== strip) {
            const had = old?.contains(document.activeElement);
            const focused = had ? (document.activeElement as HTMLElement).dataset.focus : undefined;
            old ? old.replaceWith(strip) : li.append(strip);
            // The same control gets the focus back, so typing in an editor survives a live record.
            if (had) ((focused ? byFocus(strip, focused) : null) ?? strip.querySelector<HTMLElement>('[data-focus]'))?.focus({ preventScroll: true });
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
        const box = marks.get(id);
        const boxLabel = `Select ${nameOf(s.items, id)}`;
        if (box && box.getAttribute('aria-label') !== boxLabel) box.setAttribute('aria-label', boxLabel);
        // Text, not colour alone, so it reads in both themes and to a screen reader as part of the link.
        const flag = a.querySelector<HTMLElement>('.row-flag')!;
        const text = flagText(s.overlaps.get(id));
        if (flag.textContent !== text) flag.textContent = text;
        flag.hidden = text === '';
        paintAssignee(a, assigneePerson(byId.get(id) ?? {}));
        const item = byId.get(id);
        const props = item ? properties(item).join(', ') : '';
        const leadEl = a.querySelector<HTMLElement>('.row-lead')!;
        const text2 = item ? lead(item) : id;
        if (leadEl.textContent !== text2) leadEl.textContent = text2;
        const pe = a.querySelector<HTMLElement>('.row-props')!;
        if (pe.textContent !== props) pe.textContent = props;
        paintTriage(a, item);
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
