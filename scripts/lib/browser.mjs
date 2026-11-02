// Drives a headless Chrome over the DevTools protocol and serves the repository
// root, with no dependencies. Needs Node 22 for the global WebSocket.
import { spawn } from 'node:child_process';
import { createServer } from 'node:http';
import { mkdtemp, readFile, rm } from 'node:fs/promises';
import { existsSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { extname, join, normalize, resolve } from 'node:path';

const TYPES = { '.html': 'text/html', '.json': 'application/json', '.png': 'image/png', '.js': 'text/javascript', '.mjs': 'text/javascript' };

export function findChrome() {
  const candidates = [
    process.env.CHROME,
    '/usr/bin/google-chrome',
    '/usr/bin/chromium',
    '/usr/bin/chromium-browser',
    '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome',
    '/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge',
  ].filter(Boolean);
  const found = candidates.find((p) => existsSync(p));
  if (!found) throw new Error('no Chrome found; set CHROME to a Chrome or Chromium binary');
  return found;
}

export async function serve(root) {
  const base = resolve(root);
  const server = createServer(async (req, res) => {
    const path = normalize(decodeURIComponent(new URL(req.url, 'http://x').pathname));
    const file = join(base, path);
    if (!file.startsWith(base)) { res.writeHead(403).end(); return; }
    if (path === '/favicon.ico') { res.writeHead(204).end(); return; }
    try {
      const body = await readFile(file);
      res.writeHead(200, { 'content-type': TYPES[extname(file)] || 'application/octet-stream' }).end(body);
    } catch {
      res.writeHead(404).end();
    }
  });
  await new Promise((ok) => server.listen(0, '127.0.0.1', ok));
  return { origin: `http://127.0.0.1:${server.address().port}`, close: () => new Promise((ok) => server.close(ok)) };
}

export async function launch({ width = 1280, height = 800 } = {}) {
  const dir = await mkdtemp(join(tmpdir(), 'fa-chrome-'));
  const child = spawn(
    findChrome(),
    ['--headless=new', '--disable-gpu', '--no-sandbox', '--hide-scrollbars', '--remote-debugging-port=0', `--user-data-dir=${dir}`, `--window-size=${width},${height}`, 'about:blank'],
    { stdio: 'ignore' },
  );
  const portFile = join(dir, 'DevToolsActivePort');
  for (let i = 0; i < 100 && !existsSync(portFile); i++) await new Promise((ok) => setTimeout(ok, 100));
  if (!existsSync(portFile)) { child.kill(); throw new Error('Chrome did not start'); }
  const port = (await readFile(portFile, 'utf8')).split('\n')[0];
  const targets = await (await fetch(`http://127.0.0.1:${port}/json`)).json();
  const ws = new WebSocket(targets.find((t) => t.type === 'page').webSocketDebuggerUrl);
  await new Promise((ok, no) => { ws.onopen = ok; ws.onerror = no; });

  let id = 0;
  const waiting = new Map();
  const listeners = [];
  ws.onmessage = (m) => {
    const msg = JSON.parse(m.data);
    if (msg.id && waiting.has(msg.id)) {
      const { ok, no } = waiting.get(msg.id);
      waiting.delete(msg.id);
      msg.error ? no(new Error(msg.error.message)) : ok(msg.result);
    } else if (msg.method) listeners.forEach((l) => l(msg));
  };
  const send = (method, params = {}) => new Promise((ok, no) => { const n = ++id; waiting.set(n, { ok, no }); ws.send(JSON.stringify({ id: n, method, params })); });

  await send('Runtime.enable');
  await send('Page.enable');
  await send('Log.enable');

  const page = {
    send,
    problems: [],
    async goto(url) {
      this.problems.length = 0;
      await send('Page.navigate', { url });
    },
    async eval(expression) {
      const r = await send('Runtime.evaluate', { expression, awaitPromise: true, returnByValue: true });
      if (r.exceptionDetails) throw new Error(r.exceptionDetails.exception?.description || r.exceptionDetails.text);
      return r.result.value;
    },
    async waitFor(expression, ms = 20000) {
      const end = Date.now() + ms;
      while (Date.now() < end) {
        if (await this.eval(expression).catch(() => false)) return;
        await new Promise((ok) => setTimeout(ok, 100));
      }
      throw new Error(`timed out waiting for ${expression}`);
    },
    async setViewport(width, height, deviceScaleFactor = 1) {
      await send('Emulation.setDeviceMetricsOverride', { width, height, deviceScaleFactor, mobile: false });
    },
    async click(x, y) {
      for (const type of ['mousePressed', 'mouseReleased']) await send('Input.dispatchMouseEvent', { type, x, y, button: 'left', clickCount: 1 });
    },
    async close() {
      ws.close();
      child.kill();
      await rm(dir, { recursive: true, force: true }).catch(() => {});
    },
  };
  listeners.push((msg) => {
    if (msg.method === 'Runtime.exceptionThrown') page.problems.push(msg.params.exceptionDetails.exception?.description || msg.params.exceptionDetails.text);
    // A blocked web font is not a defect of the page; any other error is.
    if (msg.method === 'Log.entryAdded' && msg.params.entry.level === 'error' && !/fonts\.(googleapis|gstatic)\.com|ERR_(INTERNET|NAME|CONNECTION)/.test(`${msg.params.entry.url || ''} ${msg.params.entry.text}`)) page.problems.push(msg.params.entry.text);
  });
  return page;
}
