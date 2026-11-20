// The Workers dialog: each supervisor with the workers it started, what each holds now, how
// the ones that ended ended, and a stop for one worker or for a whole group. Everything shown is
// a recorded field of `worker_started`, `worker_stopped`, `worker_finished` or `worker_alive`, or of the hold.
import { since } from '../activity.ts';
import type { ItemRow } from '../fold.ts';
import type { ViewState } from '../state.ts';
import { beat } from '../card.ts';
import { groups, launchText, liveCount, liveNames, STATE_TEXT, summary, type Group, type WorkerLine, type WorkerRow } from '../workers.ts';
import { modal } from './dialog.ts';
import { h } from './dom.ts';
import { itemHref } from './item-list.ts';

export interface WorkersHandlers {
  /** Opens an item in the panel, after the dialog closed. */
  open(id: string): void;
  /** Whether the viewer is signed in to the tracker, which is what a stop needs. */
  canStop(): boolean;
  /** Stops each named worker in turn; resolves to why one was refused, or null once all stopped. */
  stop(workers: string[]): Promise<string | null>;
  signIn(): void;
  now(): number;
}

function holdCell(w: WorkerLine, s: ViewState, now: number, open: (id: string) => void, close: () => void): Node {
  if (!w.hold) return h('span', { class: 'muted' }, 'Nothing held');
  const hold = w.hold;
  const link = h('a', {
    href: itemHref(hold.item, s),
    'data-item': hold.item,
    onclick: (e) => {
      const k = e as MouseEvent;
      if (k.metaKey || k.ctrlKey || k.shiftKey || k.button !== 0) return;
      e.preventDefault();
      close();
      open(hold.item);
    },
  }, hold.title ? `${hold.item} ${hold.title}` : hold.item);
  const quiet = hold.active_ms ? `last tracker write ${since(hold.active_ms, now)}` : '';
  return h('span', {}, link, h('span', { class: 'muted small' }, ` attempt ${hold.attempt}${quiet ? `, ${quiet}` : ''}`));
}

/** What the supervisor's last heartbeat says about a live worker's process; nothing for one that was never named in one. */
function beatLine(w: WorkerLine, now: number): Node | null {
  const b = w.state === 'working' || w.state === 'idle' ? beat(w.alive_ms, now) : null;
  return b ? h('div', { class: `beat small ${b.silent ? 'silent' : 'muted'}`, 'data-beat': b.silent ? 'silent' : 'running' }, b.text) : null;
}

function ended(w: WorkerLine, now: number): string {
  if (w.stopped_ms === null) return `started ${since(w.started_ms, now)}`;
  return `${w.finished ? 'finished' : 'stopped'} ${since(w.stopped_ms, now)}`;
}

export function workersView(on: WorkersHandlers) {
  const m = modal('Workers', 'workers');
  let drawn = '';
  // The group whose Stop all waits for its second click, or '*' for every group.
  let confirming: string | null = null;
  let message = '';
  let busy = false;
  let last: { rows: readonly WorkerRow[]; items: readonly ItemRow[]; s: ViewState } | null = null;
  const close = h('button', { type: 'button', class: 'button ghost', onclick: () => m.close() }, 'Close');
  const status = h('p', { class: 'muted small', role: 'status' });

  const run = async (names: string[], what: string) => {
    if (busy || names.length === 0) return;
    busy = true;
    confirming = null;
    message = `Stopping ${what}...`;
    draw(true);
    const why = await on.stop(names);
    busy = false;
    message = why ?? `Stopped ${what}.`;
    draw(true);
  };

  const stopAll = (gs: readonly Group[], supervisor: string | null, label: string) => {
    const names = liveNames(gs, supervisor);
    if (names.length === 0) return null;
    const key = supervisor ?? '*';
    const noun = `${names.length} ${names.length === 1 ? 'worker' : 'workers'}`;
    if (confirming !== key) {
      return h('button', { type: 'button', class: 'button ghost', 'data-stop-all': key, disabled: busy, onclick: () => ((confirming = key), (message = ''), draw(true)) }, label);
    }
    return h('span', { class: 'stop-confirm' },
      h('span', { class: 'small' }, `Stop ${noun}? Each token stops working and what it holds goes back.`),
      h('button', { type: 'button', class: 'button danger', 'data-confirm-stop': key, onclick: () => void run(names, noun) }, `Stop ${noun}`),
      h('button', { type: 'button', class: 'button ghost', onclick: () => ((confirming = null), draw(true)) }, 'Cancel'),
    );
  };

  const group = (g: Group, gs: readonly Group[], s: ViewState, now: number) => {
    const id = `workers-${g.supervisor}`;
    return h('section', { 'data-supervisor': g.supervisor },
      h('div', { class: 'workers-head' },
        h('h3', { id }, h('strong', { class: 'supervisor' }, g.supervisor), h('span', { class: 'muted small' }, ` started ${g.workers.length} ${g.workers.length === 1 ? 'worker' : 'workers'}: ${summary(g)}`)),
        on.canStop() ? stopAll(gs, g.supervisor, `Stop all (${g.live})`) : null,
      ),
      h('table', { class: 'spend-table workers-table', 'aria-labelledby': id },
        h('thead', {}, h('tr', {}, ...['Worker', 'State', 'Launched with', 'Holding', ''].map((c) => h('th', { scope: 'col' }, c)))),
        h('tbody', {}, ...g.workers.map((w) => {
          const live = w.state === 'working' || w.state === 'idle';
          return h('tr', { 'data-worker': w.worker, 'data-state': w.state },
            h('th', { scope: 'row' }, w.worker, h('div', { class: 'muted small' }, ended(w, now))),
            h('td', {}, h('span', { class: `worker-state ${w.state}` }, STATE_TEXT[w.state]), beatLine(w, now)),
            h('td', {}, launchText(w) || null),
            h('td', {}, holdCell(w, s, now, on.open, () => m.close())),
            h('td', { class: 'num' }, live && on.canStop()
              ? h('button', { type: 'button', class: 'button ghost', 'data-stop': w.worker, 'aria-label': `Stop ${w.worker}`, disabled: busy, onclick: () => void run([w.worker], w.worker) }, 'Stop')
              : null),
          );
        })),
      ),
    );
  };

  function draw(force = false) {
    if (!last) return;
    const { rows, items, s } = last;
    const now = on.now();
    const gs = groups(rows, items);
    // A live record that changes nothing shown must not rebuild the dialog and drop the keyboard focus.
    const key = JSON.stringify([gs, confirming, message, busy, on.canStop(), Math.floor(now / 15_000)]);
    if (!force && key === drawn) return;
    drawn = key;
    status.textContent = message;
    const wasOpen = m.isOpen();
    const focus = (document.activeElement as HTMLElement | null)?.dataset;
    const parts: (Node | null)[] = [
      h('header', { class: 'dialog-head' }, h('h2', {}, 'Workers'), close),
      gs.length === 0
        ? h('p', { class: 'muted' }, 'No supervisor has started a worker yet.')
        : h('p', { class: 'workers-total' }, h('strong', {}, String(liveCount(gs))), ` live of ${rows.length} started, under ${gs.length} ${gs.length === 1 ? 'supervisor' : 'supervisors'}`),
      !on.canStop() && gs.some((g) => g.live > 0)
        ? h('p', { class: 'muted small' }, 'Sign in to the tracker to stop a worker. ', h('button', { type: 'button', class: 'button ghost', onclick: () => (m.close(), on.signIn()) }, 'Sign in'))
        : null,
      gs.length > 1 ? stopAll(gs, null, `Stop every live worker (${liveCount(gs)})`) : null,
      ...gs.map((g) => group(g, gs, s, now)),
      status,
      h('p', { class: 'muted small spend-note' }, 'A stop ends the worker at once: its token stops working and the item it holds goes back without costing a try; Releasing is the moment between the stop and that release. A worker that reported it finished shows Finished.'),
    ];
    m.body.replaceChildren(...parts.filter((n): n is Node => n !== null));
    if (!wasOpen) return;
    // Keep the keyboard on the control it was on across a redraw.
    const again = focus?.stop ? `[data-stop="${CSS.escape(focus.stop)}"]` : focus?.stopAll ? `[data-stop-all="${CSS.escape(focus.stopAll)}"],[data-confirm-stop="${CSS.escape(focus.stopAll)}"]` : focus?.confirmStop ? `[data-confirm-stop="${CSS.escape(focus.confirmStop)}"],[data-stop-all="${CSS.escape(focus.confirmStop)}"]` : '';
    if (again) m.body.querySelector<HTMLElement>(again)?.focus();
  }

  return {
    el: m.el,
    isOpen: m.isOpen,
    /** Redraws in place if open, so ages keep up between records. */
    tick() {
      if (m.isOpen()) draw();
    },
    /** Draws the groups, or a loading note when `rows` is null, and opens the dialog; if it is open, a live record redraws it in place. */
    open(rows: readonly WorkerRow[] | null, items: readonly ItemRow[], s: ViewState) {
      if (rows === null) {
        last = null;
        drawn = '';
        m.body.replaceChildren(
          h('header', { class: 'dialog-head' }, h('h2', {}, 'Workers'), close),
          h('p', { class: 'muted', role: 'status' }, 'Still reading the history. Workers appear when it is loaded.'),
        );
        if (!m.isOpen()) {
          m.open();
          close.focus();
        }
        return;
      }
      const wasOpen = m.isOpen();
      if (!wasOpen) message = '';
      last = { rows, items, s };
      draw();
      if (wasOpen) return;
      m.open();
      close.focus();
    },
  };
}
