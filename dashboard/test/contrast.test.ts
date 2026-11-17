import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { test } from 'node:test';

// WCAG 2.2 AA on the design tokens, both themes: 1.4.3 text 4.5:1, 1.4.11 UI edges and focus 3:1.
const css = readFileSync(new URL('../src/ui/tokens.css', import.meta.url), 'utf8');

type Rgba = [number, number, number, number];

function block(selector: string): Map<string, string> {
  const start = css.indexOf(`${selector} {`);
  assert.ok(start >= 0, `${selector} block`);
  const body = css.slice(css.indexOf('{', start) + 1, css.indexOf('}', start));
  return new Map([...body.matchAll(/(--[\w-]+):\s*([^;]+);/g)].map((m) => [m[1], m[2].trim()]));
}

function parse(value: string): Rgba {
  const hex = value.match(/^#([0-9a-f]{6})$/i);
  if (hex) return [0, 2, 4].map((i) => parseInt(hex[1].slice(i, i + 2), 16)).concat(1) as Rgba;
  const rgba = value.match(/^rgba?\(([^)]+)\)$/);
  assert.ok(rgba, `a colour the check can read: ${value}`);
  const [r, g, b, a = 1] = rgba[1].split(',').map(Number);
  return [r, g, b, a];
}

// A translucent colour is what the eye sees laid over the surface behind it.
const over = (top: Rgba, under: Rgba): Rgba => {
  const a = top[3];
  return [0, 1, 2].map((i) => top[i] * a + under[i] * (1 - a)).concat(1) as Rgba;
};

const luminance = ([r, g, b]: Rgba) => {
  const lin = (c: number) => ((c /= 255) <= 0.04045 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4);
  return 0.2126 * lin(r) + 0.7152 * lin(g) + 0.0722 * lin(b);
};

function ratio(a: Rgba, b: Rgba): number {
  const [hi, lo] = [luminance(a), luminance(b)].sort((x, y) => y - x);
  return (hi + 0.05) / (lo + 0.05);
}

const dark = block(':root');
const themes: [string, Map<string, string>][] = [
  ['dark', dark],
  ['light', new Map([...dark, ...block(":root[data-theme='light']")])],
];

for (const [name, tokens] of themes) {
  const get = (t: string): Rgba => parse(tokens.get(t) ?? assert.fail(`${name}: ${t} is not defined`));
  const surfaces = ['--bg', '--panel', '--raised', '--hover'];

  for (const text of ['--text', '--muted', '--accent-ink', '--danger', '--warn', '--ok']) {
    for (const surface of surfaces) {
      test(`${name}: ${text} on ${surface} is at least 4.5:1`, () => {
        const r = ratio(get(text), get(surface));
        assert.ok(r >= 4.5, `${r.toFixed(2)}`);
      });
    }
  }

  test(`${name}: text on the accent fill and on the soft accent is at least 4.5:1`, () => {
    assert.ok(ratio(get('--on-accent'), get('--accent')) >= 4.5, 'on-accent on accent');
    for (const surface of ['--bg', '--panel']) {
      assert.ok(ratio(get('--accent-ink'), over(get('--accent-soft'), get(surface))) >= 4.5, `accent-ink on soft accent over ${surface}`);
    }
  });

  for (const surface of surfaces) {
    test(`${name}: focus ring and control edges on ${surface} are at least 3:1`, () => {
      assert.ok(ratio(get('--focus'), get(surface)) >= 3, 'focus');
      const edge = ratio(over(get('--line-strong'), get(surface)), get(surface));
      assert.ok(edge >= 3, `line-strong ${edge.toFixed(2)}`);
    });
  }
}
