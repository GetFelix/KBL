// Drives a headless Chrome over the DevTools protocol and serves the repository
// root, with no dependencies. Needs Node 22 for the global WebSocket.
import { spawn } from 'node:child_process';
import { createServer } from 'node:http';
import { mkdtemp, readFile, rm } from 'node:fs/promises';
import { existsSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { extname, join, normalize, resolve, sep } from 'node:path';

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
    if (file !== base && !file.startsWith(base + sep)) { res.writeHead(403).end(); return; }
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

// A saturated CI runner can take well over 10 s to bring Chrome up.
const STARTUP_MS = Number(process.env.CHROME_STARTUP_MS) || 60000;
const sleep = (ms) => new Promise((ok) => setTimeout(ok, ms));

export async function launch({ width = 1280, height = 800 } = {}) {
  const dir = await mkdtemp(join(tmpdir(), 'fa-chrome-'));
  const binary = findChrome();
  const child = spawn(
    binary,
    ['--headless=new', '--disable-gpu', '--no-sandbox', '--hide-scrollbars', '--remote-debugging-port=0', `--user-data-dir=${dir}`, `--window-size=${width},${height}`, 'about:blank'],
    { stdio: ['ignore', 'ignore', 'pipe'] },
  );
  // Keep the tail of stderr so a failed start can say why.
  let stderr = '';
  child.stderr.on('data', (d) => { stderr = (stderr + d).slice(-4000); });
  let spawnError = null;
  child.on('error', (e) => { spawnError = e; });
  const stopped = () => spawnError !== null || child.exitCode !== null || child.signalCode !== null;
  const closed = new Promise((ok) => child.once('close', ok));
  const abort = async (reason) => {
    const how = spawnError ? `spawn error: ${spawnError.message}` : stopped() ? `exit code ${child.exitCode}, signal ${child.signalCode}` : 'still running when killed';
    child.kill();
    // 'close' fires once stderr is drained, so the message below has Chrome's last words.
    await Promise.race([closed, sleep(2000)]);
    await rm(dir, { recursive: true, force: true }).catch(() => {});
    throw new Error(`${reason} (${binary}; ${how})${stderr.trim() ? `\nChrome stderr:\n${stderr.trim()}` : ''}`);
  };

  const portFile = join(dir, 'DevToolsActivePort');
  const deadline = Date.now() + STARTUP_MS;
  while (!existsSync(portFile)) {
    if (stopped()) await abort('Chrome exited before it was ready');
    if (Date.now() > deadline) await abort(`Chrome did not start within ${STARTUP_MS / 1000} s`);
    await sleep(100);
  }
  // The file can be seen before its contents are written; read until the port is there.
  let port;
  while (!(port = (await readFile(portFile, 'utf8')).split('\n')[0])) {
    if (Date.now() > deadline) await abort('Chrome wrote an empty DevToolsActivePort');
    await sleep(50);
  }
  let ws;
  try {
    let target;
    while (!(target = (await (await fetch(`http://127.0.0.1:${port}/json`)).json()).find((t) => t.type === 'page'))) {
      if (stopped() || Date.now() > deadline) throw new Error('Chrome has no page to drive');
      await sleep(100);
    }
    ws = new WebSocket(target.webSocketDebuggerUrl);
    await new Promise((ok, no) => { ws.onopen = ok; ws.onerror = () => no(new Error('could not connect to Chrome')); });
  } catch (e) {
    await abort(e.message);
  }

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
  const send = (method, params = {}, ms = 30000) => new Promise((ok, no) => {
    const n = ++id;
    const timer = setTimeout(() => { waiting.delete(n); no(new Error(`${method} timed out after ${ms} ms`)); }, ms);
    waiting.set(n, { ok: (v) => { clearTimeout(timer); ok(v); }, no: (e) => { clearTimeout(timer); no(e); } });
    ws.send(JSON.stringify({ id: n, method, params }));
  });
  ws.onclose = () => { for (const { no } of waiting.values()) no(new Error('Chrome closed the connection')); waiting.clear(); };

  await send('Runtime.enable');
  await send('Page.enable');
  await send('Log.enable');

  const page = {
    send,
    problems: [],
    // Resolves once the new document has loaded, so a poll cannot read the previous page.
    async goto(url) {
      this.problems.length = 0;
      const loaded = new Promise((ok) => { const l = (m) => { if (m.method === 'Page.loadEventFired') { listeners.splice(listeners.indexOf(l), 1); ok(); } }; listeners.push(l); });
      await send('Page.navigate', { url });
      await loaded;
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
    async setViewport(width, height) {
      await send('Emulation.setDeviceMetricsOverride', { width, height, deviceScaleFactor: 1, mobile: false });
    },
    async click(x, y) {
      for (const type of ['mousePressed', 'mouseReleased']) await send('Input.dispatchMouseEvent', { type, x, y, button: 'left', clickCount: 1 });
    },
    async close() {
      ws.onclose = null;
      ws.close();
      child.kill();
      await new Promise((ok) => (child.exitCode !== null ? ok() : child.once('exit', ok)));
      await rm(dir, { recursive: true, force: true }).catch(() => {});
    },
  };
  listeners.push((msg) => {
    if (msg.method === 'Runtime.consoleAPICalled' && msg.params.type === 'error') page.problems.push(msg.params.args.map((a) => a.value ?? a.description ?? '').join(' '));
    if (msg.method === 'Runtime.exceptionThrown') page.problems.push(msg.params.exceptionDetails.exception?.description || msg.params.exceptionDetails.text);
    // A blocked web font is not a defect of the page; any other error is.
    if (msg.method === 'Log.entryAdded' && msg.params.entry.level === 'error' && !/fonts\.(googleapis|gstatic)\.com|ERR_(INTERNET|NAME|CONNECTION)/.test(`${msg.params.entry.url || ''} ${msg.params.entry.text}`)) page.problems.push(msg.params.entry.text);
  });
  return page;
}
