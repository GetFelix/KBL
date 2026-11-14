import assert from 'node:assert/strict';
import { test } from 'node:test';
import { cardKey, flagText, MAX_NOTES, moreFilesText, moreNotesText, overlapCount, overlapNotes } from '../src/overlap.ts';

const card = (pairs: number) =>
  overlapNotes({ overlapsJson: () => JSON.stringify(Array.from({ length: pairs }, (_, i) => ({ first: 'r0', then: `r${i + 1}`, files: ['a.rs'] }))) }).get('r0');

test('the row flag counts every overlap, including the notes a card leaves out', () => {
  assert.equal(flagText(undefined), '');
  assert.equal(flagText(card(1)), '1 overlap');
  assert.equal(overlapCount(card(MAX_NOTES + 2)), MAX_NOTES + 2);
  assert.equal(flagText(card(MAX_NOTES + 2)), `${MAX_NOTES + 2} overlaps`);
});

test('the "+N more" lines appear only when something is left out', () => {
  assert.equal(moreNotesText(card(MAX_NOTES)!), '');
  assert.equal(moreNotesText(card(MAX_NOTES + 1)!), '+1 more overlap');
  assert.equal(moreNotesText(card(MAX_NOTES + 3)!), '+3 more overlaps');
  const note = { other: 'r1', files: ['a.rs'], moreFiles: 0, suggest: 'r1 waits on r0' };
  assert.equal(moreFilesText(note), '');
  assert.equal(moreFilesText({ ...note, moreFiles: 4 }), '+4 more');
});

test('the panel key changes with the notes and is empty without them', () => {
  assert.equal(cardKey(undefined), '');
  assert.notEqual(cardKey(card(1)), cardKey(card(2)));
  assert.equal(cardKey(card(2)), cardKey(card(2)));
});
