import assert from 'node:assert/strict';
import { test } from 'node:test';
import { COMMANDS, commandFor, displayChord, type KeyPress } from '../src/keys.ts';

const press = (key: string, more: Partial<KeyPress> = {}): KeyPress => ({ key, metaKey: false, ctrlKey: false, altKey: false, inText: false, ...more });

test('the palette opens with Cmd+K on a Mac and Ctrl+K elsewhere, and not with the other modifier', () => {
  assert.equal(commandFor(press('k', { metaKey: true }), true), 'palette');
  assert.equal(commandFor(press('k', { ctrlKey: true }), false), 'palette');
  assert.equal(commandFor(press('k', { ctrlKey: true }), true), undefined);
  assert.equal(commandFor(press('K', { metaKey: true, inText: true }), true), 'palette');
});

test('the common single keys mean what they mean in Linear and GitHub', () => {
  assert.equal(commandFor(press('j'), false), 'next');
  assert.equal(commandFor(press('k'), false), 'previous');
  assert.equal(commandFor(press('/'), false), 'search');
  assert.equal(commandFor(press('?'), false), 'help');
  assert.equal(commandFor(press('Escape'), false), 'close');
});

test('plain letters type into a text field instead of running commands', () => {
  for (const key of ['j', 'k', '/', '?']) assert.equal(commandFor(press(key, { inText: true }), false), undefined, key);
  assert.equal(commandFor(press('Escape', { inText: true }), false), 'close');
  assert.equal(commandFor(press('ArrowDown', { inText: true }), false), 'next');
});

test('Alt chords and unknown keys do nothing', () => {
  assert.equal(commandFor(press('j', { altKey: true }), false), undefined);
  assert.equal(commandFor(press('q'), false), undefined);
});

test('no key is bound to two commands', () => {
  const seen = new Map<string, string>();
  for (const c of COMMANDS) for (const k of c.keys) {
    assert.ok(!seen.has(k.toLowerCase()), `${k} is bound to ${seen.get(k.toLowerCase())} and ${c.id}`);
    seen.set(k.toLowerCase(), c.id);
  }
});

test('Mod reads as the platform modifier', () => {
  assert.deepEqual(displayChord('Mod+K', true), ['⌘', 'K']);
  assert.deepEqual(displayChord('Mod+K', false), ['Ctrl', 'K']);
  assert.deepEqual(displayChord('Escape', false), ['Esc']);
});
