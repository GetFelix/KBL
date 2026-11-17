// The Metrics dialog: team numbers read from the tracker. Today that is how often the
// summaries people marked were accurate. It has no per-person or per-agent figure.
import { trustText, type Trust } from '../trust.ts';
import { modal } from './dialog.ts';
import { h } from './dom.ts';

export const WINDOWS: readonly number[] = [7, 30, 90];

export interface MetricsHandlers {
  /** Whether the viewer is signed in to the tracker, which is what the numbers come from. */
  signedIn(): boolean;
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
    if (typeof t === 'string') card.replaceChildren(h('p', { class: 'metrics-error' }, t));
    else draw(t);
  };

  return {
    el: m.el,
    isOpen: m.isOpen,
    open() {
      windows.value = String(days);
      m.body.replaceChildren(
        h('header', { class: 'dialog-head' }, h('h2', {}, 'Metrics'), close),
        h('section', { 'aria-labelledby': 'metrics-trust' },
          h('h3', { id: 'metrics-trust' }, 'Trust in summaries'),
          h('p', { class: 'muted small' }, 'Of the agent summaries a person marked, how many were marked accurate. Each attempt counts once, by its latest mark. A team figure: nothing here names an agent, a model or who marked.'),
          h('p', {}, windows),
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
