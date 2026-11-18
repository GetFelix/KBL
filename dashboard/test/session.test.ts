import assert from 'node:assert/strict';
import { beforeEach, test } from 'node:test';
import { forgetGateway, forgetPerson, loadGateway, loadPerson, saveGateway, savePerson } from '../src/session.ts';

const backing = new Map<string, string>();
const local = new Map<string, string>();
const area = (m: Map<string, string>) => ({ getItem: (k: string) => m.get(k) ?? null, setItem: (k: string, v: string) => void m.set(k, v), removeItem: (k: string) => void m.delete(k) });
Object.assign(globalThis, { sessionStorage: area(backing), localStorage: area(local) });

beforeEach(() => {
  backing.clear();
  local.clear();
});

test('a gateway session and a person token survive in session storage only', () => {
  saveGateway({ gateway: 'ws://g/ws', room: 'kbl', token: 'gw-secret' });
  savePerson('person-secret');
  assert.deepEqual(loadGateway(), { gateway: 'ws://g/ws', room: 'kbl', token: 'gw-secret' });
  assert.equal(loadPerson(), 'person-secret');
  assert.equal(local.size, 0, 'nothing reaches localStorage');
});

test('forgetting clears each one', () => {
  saveGateway({ gateway: null, room: 'kbl', token: 't' });
  savePerson('p');
  forgetPerson();
  assert.equal(loadPerson(), null);
  assert.notEqual(loadGateway(), null);
  forgetGateway();
  assert.equal(loadGateway(), null);
});

test('a damaged saved session reads as none', () => {
  backing.set('kbl.session.gateway', '{"room":1}');
  assert.equal(loadGateway(), null);
  backing.set('kbl.session.gateway', 'not json');
  assert.equal(loadGateway(), null);
});

test('storage that throws is not fatal', () => {
  const boom = { getItem: () => { throw new Error('blocked'); }, setItem: () => { throw new Error('blocked'); }, removeItem: () => { throw new Error('blocked'); } };
  Object.assign(globalThis, { sessionStorage: boom });
  saveGateway({ gateway: null, room: 'r', token: 't' });
  assert.equal(loadGateway(), null);
  assert.equal(loadPerson(), null);
  Object.assign(globalThis, { sessionStorage: area(backing) });
});
