// The Metrics dialog: team numbers. Flow, rework, cost and supervision come from the log the
// page has folded; trust in summaries is read from the tracker. None is per person or per agent.
import { metricsText, type MetricsReport, type Row } from '../metrics.ts';
import { trustText, type Trust } from '../trust.ts';
import { modal } from './dialog.ts';
import { h } from './dom.ts';

export const WINDOWS: readonly number[] = [7, 30, 90];

export interface MetricsHandlers {
  /** Whether the viewer is signed in to the tracker, which is what the numbers come from. */
  signedIn(): boolean;
  /** The numbers folded from the log over `days`, or null before the log has finished loading. */
  report(days: number): MetricsReport | null;
  /** The marks over `days`, or why the tracker would not say. */
  load(days: number): Promise<Trust | string>;
  signIn(): void;
}

export function metricsView(on: MetricsHandlers) {
  const m = modal('Metrics', 'metrics');
  const close = h('button', { type: 'button', class: 'button ghost', onclick: () => m.close() }, 'Close');
  let days = 30;
  // A slower answer for an earlier window must not draw over a later pick.
  let latest = 0;
  const card = h('div', { class: 'metrics-card', role: 'status' });
  const windows = h('select', { 'aria-label': 'Time window', onchange: (e) => void show(Number((e.target as HTMLSelectElement).value)) },
    ...WINDOWS.map((d) => h('option', { value: String(d) }, d === 1 ? 'Last day' : `Last ${d} days`)));

  const flow = h('div', { class: 'metrics-flow' });
  const table = (caption: string, head: string[], rows: Row[], empty: string) =>
    h('table', { class: 'metrics-table' },
      h('caption', { class: 'sr-only' }, caption),
      h('thead', {}, h('tr', {}, ...head.map((c, i) => h('th', { scope: 'col', class: i ? 'num' : undefined }, c)))),
      h('tbody', {}, ...(rows.length ? rows.map((r) => h('tr', {}, h('th', { scope: 'row' }, r.label), ...r.cells.map((c) => h('td', { class: 'num' }, c)))) : [h('tr', {}, h('td', { colspan: String(head.length) }, empty))])));
  const section = (id: string, title: string, ...body: (Node | string | null)[]) =>
    h('section', { 'aria-labelledby': id }, h('h3', { id }, title), ...body);

  const drawFlow = (r: MetricsReport | null) => {
    if (r === null) {
      flow.replaceChildren(h('p', { class: 'muted', role: 'status' }, 'The log is still loading, so there are no flow numbers yet. Reopen Metrics once it has.'));
      return;
    }
    const x = metricsText(r);
    const peak = Math.max(1, ...x.throughput.perDay);
    const parts = [
      x.empty ? h('p', { class: 'muted', role: 'status' }, x.empty) : null,
      x.clockBehind ? h('p', { class: 'metrics-warning small', role: 'status' }, x.clockBehind) : null,
      x.partial ? h('p', { class: 'metrics-warning small' }, x.partial) : null,
      section('metrics-stages', 'Time in each stage',
        h('p', { class: 'muted small' }, `How long items stayed in a stage before leaving it, for stays that ended in ${x.window}. p50 is the middle, p85 the slower end.`),
        table('Time in each stage', ['Stage', 'Stays', 'p50', 'p85'], x.stages, 'No stays.')),
      section('metrics-throughput', 'Throughput',
        h('p', {}, x.throughput.summary),
        h('div', { class: 'metrics-bars', role: 'img', 'aria-label': x.throughput.dayLabel },
          ...x.throughput.perDay.map((n) => h('span', { class: 'metrics-bar', style: `height:${Math.round((n / peak) * 100)}%` }))),
        table('Items done per week', ['Week', 'Done'], x.throughput.weeks, 'Nothing done.')),
      section('metrics-rework', 'Rework',
        h('p', { class: 'muted small' }, 'Of the items done in this window. A redo forks an attempt, reopened is a closed item opened again, a failed attempt is one an agent gave up or released as failed.'),
        table('Rework', ['Kind', 'Count', 'Per done item'], x.rework, 'None.')),
      section('metrics-cost', 'Cost',
        h('p', {}, x.cost.summary),
        x.cost.note ? h('p', { class: 'muted small' }, x.cost.note) : null,
        table('Cost per model', ['Model', 'Cost', 'Per done item'], x.cost.rows, 'No usage was recorded by model.')),
      section('metrics-supervision', 'Supervision',
        h('p', { class: 'muted small' }, 'How much a person had to step in, for the items done in this window.'),
        table('Supervision', ['Measure', 'Value', 'Detail'], x.supervision, 'None.')),
    ];
    flow.replaceChildren(...parts.filter((n) => n !== null));
  };

  const draw = (t: Trust) => {
    const x = trustText(t);
    const lines = [
      h('p', { class: 'metrics-headline' }, x.headline),
      x.share ? h('p', { class: x.small ? 'metrics-share muted' : 'metrics-share', 'data-small': x.small ? 'true' : undefined }, x.share) : null,
      x.warning ? h('p', { class: 'metrics-warning small' }, x.warning) : null,
      h('p', { class: 'muted small' }, x.unrated),
      x.undated ? h('p', { class: 'muted small' }, x.undated) : null,
    ];
    card.replaceChildren(...lines.filter((n) => n !== null));
  };

  const show = async (d: number) => {
    days = d;
    drawFlow(on.report(d));
    const mine = ++latest;
    if (!on.signedIn()) {
      card.replaceChildren(
        h('p', { class: 'muted' }, 'The numbers come from the tracker. Sign in to read them.'),
        h('button', { type: 'button', class: 'button', onclick: () => (m.close(), on.signIn()) }, 'Sign in'),
      );
      return;
    }
    card.replaceChildren(h('p', { class: 'muted' }, 'Reading the tracker.'));
    const t = await on.load(d);
    if (mine !== latest) return;
    if (typeof t === 'string') card.replaceChildren(h('p', { class: 'metrics-error' }, t), h('button', { type: 'button', class: 'button', onclick: () => void show(days) }, 'Try again'));
    else draw(t);
  };

  return {
    el: m.el,
    isOpen: m.isOpen,
    open() {
      windows.value = String(days);
      m.body.replaceChildren(
        h('header', { class: 'dialog-head' }, h('h2', {}, 'Metrics'), close),
        h('p', {}, windows),
        flow,
        h('section', { 'aria-labelledby': 'metrics-trust' },
          h('h3', { id: 'metrics-trust' }, 'Trust in summaries'),
          h('p', { class: 'muted small' }, 'Of the agent summaries a person marked, how many were marked accurate. Each attempt counts once, by its latest mark. A team figure: nothing here names an agent, a model or who marked.'),
          card,
        ),
      );
      void show(days);
      if (m.isOpen()) return;
      m.open();
      close.focus();
    },
  };
}
