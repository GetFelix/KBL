import assert from 'node:assert/strict';
import { mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';
import { pathToFileURL } from 'node:url';
import { build } from 'vite';

// A WebSocket that answers `join` with `hello` and every other frame with an echo, and reports its close.
class FakeSocket {
  onopen: (() => void) | null = null;
  onmessage: ((e: { data: string }) => void) | null = null;
  onclose: (() => void) | null = null;
  onerror: (() => void) | null = null;
  constructor() {
    setTimeout(() => this.onopen?.(), 0);
  }
  send(text: string) {
    const m = JSON.parse(text);
    const reply = m.type === 'join' ? { type: 'hello', features: ['read'] } : { type: 'echo', id: m.id };
    setTimeout(() => this.onmessage?.({ data: JSON.stringify(reply) }), 0);
  }
  close() {
    setTimeout(() => this.onclose?.(), 0);
  }
}

// The page runs gateway.ts as Vite bundles it, so the test does too: a bundler that
// decides the transport's handlers are the empty defaults drops the call that hands
// messages to follow, and the page never gets past reading history.
test('the bundled connect hands messages and the close after hello to the handlers set later', async () => {
  const out = (await build({
    configFile: false,
    logLevel: 'silent',
    build: {
      write: false,
      minify: 'esbuild',
      lib: { entry: new URL('../src/gateway.ts', import.meta.url).pathname, formats: ['es'], fileName: 'gateway' },
    },
  })) as { output: { code?: string }[] }[];
  const dir = mkdtempSync(join(tmpdir(), 'kbl-gateway-'));
  try {
    const file = join(dir, 'gateway.mjs');
    writeFileSync(file, out[0].output[0].code ?? '');
    const { connect } = await import(pathToFileURL(file).href);
    (globalThis as { WebSocket?: unknown }).WebSocket = FakeSocket;
    const transport = await connect('ws://fake', 'room', 'token');
    const got = new Promise((ok) => (transport.onMessage = ok));
    transport.send({ type: 'read', id: 7 });
    assert.deepEqual(await got, { type: 'echo', id: 7 });
    const closed = new Promise((ok) => (transport.onClose = ok));
    transport.close();
    assert.equal(await closed, 'closed');
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});
