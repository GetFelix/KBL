// What the dev-stack checks share: a token, a run, and agents that are felix-mcp
// processes spoken to over stdio, as under an MCP host.
import { spawn, spawnSync } from "node:child_process";
import { mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createInterface } from "node:readline";

const env = (name, fallback) => process.env[name] || fallback;
const CONTROL_PLANE = env("AGENTS_FELIX_CONTROL_PLANE", "http://127.0.0.1:8443");
const IDP = env("AGENTS_IDP", "http://127.0.0.1:9400");
const MCP = env("FELIX_MCP", "target/debug/felix-mcp");
const TOOL = env("AGENTS_TOOL", "target/debug/agents-tool");
const TRACKER = env("AGENTS_TRACKER", "target/debug/agents-tracker");
const TRACKER_ADDR = env("AGENTS_TRACKER_LISTEN", "127.0.0.1:8790");
const TENANT = "agents";

const running = new Set();

async function json(url, init) {
  const response = await fetch(url, init);
  if (!response.ok) throw new Error(`${url} -> ${response.status}: ${await response.text()}`);
  return response.json();
}

// No agent identities exist yet, so the tenant's administrator stands in for every agent.
export async function felixToken() {
  const idToken = (await json(`${IDP}/token?sub=agents-admin&aud=felix-agents`)).id_token;
  const exchanged = await json(`${CONTROL_PLANE}/v1/tenants/${TENANT}/token/exchange`, {
    method: "POST",
    headers: { "content-type": "application/json", authorization: `Bearer ${idToken}` },
    body: JSON.stringify({ audience: "felix-broker" }),
  });
  return exchanged.felix_token;
}

export function createRun(run, goal) {
  const created = spawnSync(TOOL, ["run", "create", run, "--goal", goal, "--budget-usd", "1"], { stdio: "inherit" });
  if (created.status !== 0) throw new Error(`could not create run ${run}`);
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

// One felix-mcp process for `name`. `tool` returns the tool's JSON reply or
// throws the text the model would have seen.
export async function agent(token, run, name, extraEnv = {}) {
  const child = spawn(MCP, [], {
    env: { ...process.env, AGENTS_RUN: run, AGENTS_AGENT: name, AGENTS_FELIX_TOKEN: token, ...extraEnv },
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
export async function startTracker(tokens, extraEnv = {}) {
  const dir = mkdtempSync(join(tmpdir(), "tracker-"));
  const tokensFile = join(dir, "tokens.json");
  writeFileSync(tokensFile, JSON.stringify(tokens));
  const child = spawn(TRACKER, [], {
    env: {
      ...process.env,
      AGENTS_TRACKER_TOKENS_FILE: tokensFile,
      AGENTS_TRACKER_LISTEN: TRACKER_ADDR,
      AGENTS_CLAIM_VISIBILITY_MS: "4000",
      ...extraEnv,
    },
    stdio: ["ignore", "inherit", "inherit"],
  });
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
