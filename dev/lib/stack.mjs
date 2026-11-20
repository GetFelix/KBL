// What the dev-stack checks share: a token, a run, and agents that are kbl-mcp
// processes spoken to over stdio, as under an MCP host.
import { spawn, spawnSync } from "node:child_process";
import { mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createInterface } from "node:readline";

const env = (name, fallback) => process.env[name] || fallback;
const CONTROL_PLANE = env("KBL_FELIX_CONTROL_PLANE", "http://127.0.0.1:8443");
const IDP = env("KBL_IDP", "http://127.0.0.1:9400");
const MCP = env("KBL_MCP", "target/debug/kbl-mcp");
const TOOL = env("KBL_TOOL", "target/debug/kbl-tool");
const TRACKER = env("KBL_TRACKER", "target/debug/kbl-tracker");
const TRACKER_ADDR = env("KBL_TRACKER_LISTEN", "127.0.0.1:8790");
const TENANT = "agents";

const running = new Set();

// Each KBL_ name also under its old AGENTS_ name, which the build the upgrade
// check starts from still reads.
export function withLegacyNames(env) {
  const out = { ...env };
  for (const [key, value] of Object.entries(env)) {
    if (key.startsWith("KBL_")) out[`AGENTS_${key.slice(4)}`] ??= value;
  }
  return out;
}

async function json(url, init) {
  const response = await fetch(url, init);
  if (!response.ok) throw new Error(`${url} -> ${response.status}: ${await response.text()}`);
  return response.json();
}

// No agent identities exist yet, so the tenant's administrator stands in for every agent.
export async function idToken(sub = "agents-admin") {
  return (await json(`${IDP}/token?sub=${encodeURIComponent(sub)}&aud=kbl`)).id_token;
}

export async function felixToken() {
  const id = await idToken();
  const exchanged = await json(`${CONTROL_PLANE}/v1/tenants/${TENANT}/token/exchange`, {
    method: "POST",
    headers: { "content-type": "application/json", authorization: `Bearer ${id}` },
    body: JSON.stringify({ audience: "felix-broker" }),
  });
  return exchanged.felix_token;
}

// A broker that has just started may not hold its shards' leases yet and
// refuses with "lease lapsed" until it does, so a call made right after
// dev/up.sh is tried again for up to two minutes.
const START_TRIES = 60;
const START_PAUSE_MS = 2000;

export function createRun(run, goal) {
  const created = spawnSync(TOOL, ["run", "create", run, "--goal", goal, "--budget-usd", "1"], { stdio: "inherit" });
  if (created.status !== 0) throw new Error(`could not create run ${run}`);
}

// A create that fails partway leaves the run's record behind and refuses its
// id from then on, so each attempt takes a new id. Returns the id that worked.
export async function createRunWhenReady(prefix, goal) {
  return whenReady(`create a ${prefix} run`, async () => {
    const run = newRunId(prefix);
    createRun(run, goal);
    return run;
  });
}

// `call` again until it stops failing, for what a fresh stack answers before its leases are held.
export async function whenReady(what, call) {
  for (let tries = 1; ; tries++) {
    try {
      return await call();
    } catch (e) {
      if (tries === START_TRIES) throw new Error(`${what}: ${e.message}`);
      await sleep(START_PAUSE_MS);
    }
  }
}

// The run's log as the records it holds, oldest first.
export function runLog(run) {
  const read = spawnSync(TOOL, ["run", "log", run], { encoding: "utf8" });
  if (read.status !== 0) throw new Error(`could not read the log of ${run}: ${read.stderr}`);
  return read.stdout
    .split("\n")
    .filter(Boolean)
    .map((line) => JSON.parse(line));
}

export function newRunId(prefix) {
  return `${prefix}-${Date.now().toString(36)}`;
}

// One kbl-mcp process for `name`. `tool` returns the tool's JSON reply or
// throws the text the model would have seen.
export async function agent(token, run, name, extraEnv = {}, bin = MCP) {
  const child = spawn(bin, [], {
    env: withLegacyNames({ ...process.env, KBL_RUN: run, KBL_AGENT: name, KBL_FELIX_TOKEN: token, ...extraEnv }),
    stdio: ["pipe", "pipe", "inherit"],
  });
  running.add(child);
  child.stdin.on("error", () => {});
  const lines = createInterface({ input: child.stdout })[Symbol.asyncIterator]();
  const exited = new Promise((resolve) => child.on("exit", (code) => resolve(`${name} exited with ${code}`)));
  let nextId = 1;
  const send = (message) => child.stdin.write(`${JSON.stringify({ jsonrpc: "2.0", ...message })}\n`);
  async function call(method, params) {
    const id = nextId++;
    send({ id, method, params });
    for (;;) {
      const next = await Promise.race([lines.next(), exited.then((why) => Promise.reject(new Error(why)))]);
      if (next.done) throw new Error(`${name} closed its output`);
      const reply = JSON.parse(next.value);
      if (reply.id !== id) continue;
      if (reply.error) throw new Error(`${name} ${method}: ${reply.error.message}`);
      return reply.result;
    }
  }
  await call("initialize", {
    protocolVersion: "2025-06-18",
    capabilities: {},
    clientInfo: { name: "dev-check", version: "0" },
  });
  send({ method: "notifications/initialized" });
  return {
    name,
    async tool(tool, args) {
      const result = await call("tools/call", { name: tool, arguments: args });
      const text = result.content[0]?.text ?? "";
      if (result.isError) throw new Error(`${name} ${tool}: ${text}`);
      return JSON.parse(text);
    },
    kill() {
      child.kill("SIGKILL");
      running.delete(child);
    },
  };
}

export function killAll() {
  for (const child of running) child.kill("SIGKILL");
}

// The work tracker as a process on the host, against the dev stack. `tokens` is
// {agents: {token: name}, people: {token: name}}. `exited` settles when it ends.
// `bin` runs another build of the tracker, as the upgrade check does.
export async function startTracker(tokens, extraEnv = {}, bin = TRACKER) {
  const dir = mkdtempSync(join(tmpdir(), "tracker-"));
  const tokensFile = join(dir, "tokens.json");
  writeFileSync(tokensFile, JSON.stringify(tokens), { mode: 0o600 });
  const env = {
      ...process.env,
      KBL_TRACKER_TOKENS_FILE: tokensFile,
      KBL_TRACKER_LISTEN: TRACKER_ADDR,
      KBL_CLAIM_VISIBILITY_MS: "4000",
      KBL_TRACKER_LEASE_TTL_MS: "3000",
      // The tracker's default review limit (3) would pause fix claims partway through a check that finishes many fixes.
      KBL_REVIEW_LIMIT: "0",
      // The checks were written for a log that starts at format 1 and raise it themselves;
      // "current" asks for the tracker's own default, a log that starts at the current format.
      KBL_FRESH_LOG_FORMAT: "1",
      ...extraEnv,
  };
  if (env.KBL_FRESH_LOG_FORMAT === "current") delete env.KBL_FRESH_LOG_FORMAT;
  const child = spawn(bin, [], { env: withLegacyNames(env), stdio: ["ignore", "inherit", "inherit"] });
  running.add(child);
  const exited = new Promise((resolve) => child.on("exit", () => resolve()));
  for (let tries = 0; ; tries++) {
    const up = await fetch(`${TRACKER_URL}/healthz`).then((r) => r.ok, () => false);
    if (up) break;
    if (tries > 60 || child.exitCode !== null) throw new Error("the tracker did not start");
    await sleep(500);
  }
  return {
    exited,
    kill() {
      child.kill("SIGKILL");
      running.delete(child);
    },
  };
}

export const TRACKER_URL = `http://${TRACKER_ADDR}`;

// Raises the shared log's format to `to`, or leaves it when it is already there or higher, so
// the order of checks that share a log does not matter (a raise below the current format is refused).
export async function raiseFormat(token, to) {
  const have = workLog().reduce((n, r) => (r.kind === "format_raised" ? Math.max(n, r.to) : n), 0);
  if (have >= to) return;
  await personRecord(token, { kind: "format_raised", to });
}

// A call to the tracker as a person; throws the tracker's error text.
export async function personRecord(token, record) {
  const response = await fetch(`${TRACKER_URL}/v1/work/records`, {
    method: "POST",
    headers: { "content-type": "application/json", authorization: `Bearer ${token}` },
    body: JSON.stringify({ record }),
  });
  const body = await response.json();
  if (!response.ok) throw new Error(`record ${record.kind}: ${body.error}`);
  return body;
}

// A person's call to one of the tracker's dashboard routes. Throws on a refusal,
// with the tracker's reason.
export async function personCall(token, path, body) {
  const response = await fetch(`${TRACKER_URL}${path}`, {
    method: "POST",
    headers: { "content-type": "application/json", authorization: `Bearer ${token}` },
    body: JSON.stringify(body),
  });
  const reply = await response.json().catch(() => ({}));
  if (!response.ok) throw new Error(`${path}: ${reply.why ?? reply.error ?? response.status}`);
  return reply;
}

// work.events as the records it holds, oldest first.
export function workLog() {
  const read = spawnSync(TOOL, ["work", "log"], { encoding: "utf8" });
  if (read.status !== 0) throw new Error(`could not read work.events: ${read.stderr}`);
  return read.stdout
    .split("\n")
    .filter(Boolean)
    .map((line) => JSON.parse(line));
}

export function check(ok, what) {
  if (!ok) throw new Error(`failed: ${what}`);
  console.log(`ok   ${what}`);
}

// Ends the process, and every agent, if a check hangs.
export function giveUpAfter(seconds, what) {
  setTimeout(() => {
    console.error(`${what} did not finish in ${seconds} seconds`);
    killAll();
    process.exit(1);
  }, seconds * 1000).unref();
}

export const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
