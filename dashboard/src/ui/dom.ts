// A tiny element builder, so views are short and never build HTML from strings
// (titles come from upstream issues and must not be parsed as markup).

type Attrs = Record<string, string | boolean | undefined | ((e: Event) => void)>;
type Child = Node | string | null | undefined | false;

export function h<K extends keyof HTMLElementTagNameMap>(tag: K, attrs: Attrs = {}, ...children: Child[]): HTMLElementTagNameMap[K] {
  const el = document.createElement(tag);
  for (const [k, v] of Object.entries(attrs)) {
    if (typeof v === 'function') el.addEventListener(k.slice(2), v);
    else if (v === true) el.setAttribute(k, '');
    else if (typeof v === 'string') el.setAttribute(k, v);
  }
  for (const c of children) if (c) el.append(c);
  return el;
}

/** Keyboard chords as `<kbd>` elements. */
export function kbd(parts: string[]): HTMLElement {
  return h('span', { class: 'chord' }, ...parts.map((p) => h('kbd', {}, p)));
}
