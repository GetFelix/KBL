import assert from 'node:assert/strict';
import { test } from 'node:test';
import { gatewayHost, pickSession, validGateway } from '../src/gateway-url.ts';

const saved = { gateway: 'wss://good.example/ws', room: 'kbl', token: 'secret' };
const q = (s: string) => new URLSearchParams(s);

test('a link with a gateway and no token keeps the saved gateway', () => {
  const p = pickSession(q('gateway=wss://evil.example/ws&room=kbl'), saved);
  assert.deepEqual(p, { kind: 'session', gateway: 'wss://good.example/ws', room: 'kbl', token: 'secret', fresh: false });
});

test('a gateway with no token and no saved session connects to nothing', () => {
  assert.deepEqual(pickSession(q('gateway=wss://evil.example/ws&room=kbl'), null), { kind: 'none' });
});

test('a link with gateway, room and token is a new session', () => {
  const p = pickSession(q('gateway=wss://new.example/ws&room=r&token=t2'), saved);
  assert.deepEqual(p, { kind: 'session', gateway: 'wss://new.example/ws', room: 'r', token: 't2', fresh: true });
});

test('schemes other than wss, and ws off this machine, are refused', () => {
  for (const g of ['http://127.0.0.1/ws', 'https://a/ws', 'javascript:alert(1)', 'ws://evil.example/ws', 'wss://u:p@a/ws', 'nonsense']) {
    assert.equal(pickSession(q(`gateway=${encodeURIComponent(g)}&room=r&token=t`), saved).kind, 'refused', g);
  }
  assert.equal(validGateway('ws://localhost:8787/ws'), 'ws://localhost:8787/ws');
  assert.equal(validGateway('ws://127.0.0.1:8787/ws'), 'ws://127.0.0.1:8787/ws');
  assert.equal(validGateway('wss://gw.example/ws'), 'wss://gw.example/ws');
});

test('a saved session with a bad gateway is not used', () => {
  assert.deepEqual(pickSession(q(''), { ...saved, gateway: 'ws://evil.example/ws' }), { kind: 'none' });
});

test('a link without a gateway uses the default for a fresh session and the saved one otherwise', () => {
  assert.equal((pickSession(q('room=r&token=t'), saved) as { gateway: string | null }).gateway, null);
  assert.equal((pickSession(q(''), { ...saved, gateway: null }) as { gateway: string | null }).gateway, null);
});

test('the host is shown for the default and for a named gateway', () => {
  assert.equal(gatewayHost(null), '127.0.0.1:8787');
  assert.equal(gatewayHost('wss://gw.example/ws'), 'gw.example');
});
