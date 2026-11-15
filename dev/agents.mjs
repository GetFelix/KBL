#!/usr/bin/env node
// Runs N MCP host sessions that each take work until the queue is empty. KBL makes
// no model call: this starts the host the person already has (Claude Code by
// default), once per session, with an .mcp.json that points at kbl-mcp with that
// session's own tokens. See "Running agents in a loop" in the README.
//
//   node dev/agents.mjs --run demo --tokens tokens.json --ca-file dev/state/broker-cert.pem
//
// A session that exits without saying the queue is empty is started again.
import { spawn } from "node:child_process";
import { mkdirSync, mkdtempSync, readdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { createInterface } from "node:readline";
import { parseArgs } from "node:util";

// What a session prints, on a line of its own, when work_next says nothing is ready.
export const EMPTY_MARKER = "KBL_QUEUE_EMPTY";

export const LOOP_PROMPT = `Use the kbl tools to take work, and keep going until there is none.
1. Call work_next. If it says nothing is ready, print ${EMPTY_MARKER} on a line of its own and stop.
2. Work the item the reply gives you until it is done or you have a question for a person: follow the rules in the reply, run its checks, and call work_complete. If you cannot finish it, call work_release with the reason.
3. Call work_next again.
Stop when nothing is ready or when you are asked to stop. Do not wait for more work to appear.`;

const USAGE = `usage: node dev/agents.mjs --run <run> (--tokens <file> | --agent <name>=<felix token>,<tracker token> ...)
  --sessions <n>        start the first n agents (default: all)
  --ca-file <path>      the broker certificate, as an absolute path
  --tracker-url <url>   the tracker (default: KBL_TRACKER_URL)
  --mcp <path>          the kbl-mcp binary (default: target/debug/kbl-mcp)
  --host <command>      the host to start (default: claude); it is run as
                        <command> [--host-arg ...] <prompt> in a directory holding .mcp.json,
                        with KBL_MCP_CONFIG naming that file
  --host-arg <arg>      an argument before the prompt; repeatable. Without --host, the Claude Code defaults are used
  --restart-delay <s>   wait before restarting a session that exited early (default: 5)
  --max-restarts <n>    give up on a session after n early exits in a row (default: 10)
  --idle-delay <s>      wait before asking again after a session found nothing (default: 15); doubles
                        for each empty answer in a row
  --idle-max <s>        longest idle wait (default: 300)

The tokens file is JSON: {"w1": {"felix": "...", "tracker": "..."}, ...}. Prefer it to --agent,
which puts tokens in the process list.`;

function readOptions(argv) {
  const { values } = parseArgs({
    args: argv,
    options: {
      run: { type: "string" },
      tokens: { type: "string" },
      agent: { type: "string", multiple: true },
      sessions: { type: "string" },
      "ca-file": { type: "string" },
      "tracker-url": { type: "string" },
      mcp: { type: "string" },
      host: { type: "string" },
      "host-arg": { type: "string", multiple: true },
      "restart-delay": { type: "string", default: "5" },
      "idle-delay": { type: "string", default: "15" },
      "idle-max": { type: "string", default: "300" },
      "max-restarts": { type: "string", default: "10" },
      help: { type: "boolean", short: "h" },
    },
  });
  if (values.help) throw new Error(USAGE);
  if (!values.run) throw new Error(`--run is required\n${USAGE}`);
  const agents = values.tokens ? JSON.parse(readFileSync(values.tokens, "utf8")) : {};
  for (const spec of values.agent ?? []) {
    const at = spec.indexOf("=");
    const name = at < 0 ? spec : spec.slice(0, at);
    const rest = at < 0 ? "" : spec.slice(at + 1);
    const [felix, tracker] = rest.split(",");
    if (!name || !felix || !tracker) throw new Error(`--agent wants <name>=<felix token>,<tracker token>, got "${name}"`);
    agents[name] = { felix, tracker };
  }
  let names = Object.keys(agents);
  if (values.sessions) {
    const n = Number(values.sessions);
    if (!Number.isInteger(n) || n < 1) throw new Error("--sessions wants a whole number of at least 1");
    names = names.slice(0, n);
  }
  if (names.length === 0) throw new Error(`no agents: give --tokens or --agent\n${USAGE}`);
  for (const name of names) {
    if (!agents[name].felix || !agents[name].tracker) throw new Error(`agent ${name} needs a felix and a tracker token`);
  }
  // The tracker takes an agent's name from its token, so sessions sharing one would all be a single agent.
  if (new Set(names.map((n) => agents[n].tracker)).size !== names.length) {
    throw new Error("two sessions share a tracker token; each session needs its own");
  }
  if (new Set(names.map((n) => agents[n].felix)).size !== names.length) {
    console.error("warning: sessions share a Felix token; per-agent tokens are not issued yet, so this is only right for a dev stack");
  }
  const idleMax = Number(values["idle-max"]);
  if (!(idleMax >= 0)) throw new Error("--idle-max wants a number of seconds");
  return {
    run: values.run,
    agents: names.map((name) => ({ name, ...agents[name] })),
    caFile: values["ca-file"] && resolve(values["ca-file"]),
    trackerUrl: values["tracker-url"] ?? process.env.KBL_TRACKER_URL,
    mcp: resolve(values.mcp ?? "target/debug/kbl-mcp"),
    host: values.host,
    hostArgs: values["host-arg"] ?? [],
    restartMs: Number(values["restart-delay"]) * 1000,
    idleMs: Number(values["idle-delay"]) * 1000,
    idleMaxMs: idleMax * 1000,
    maxRestarts: Number(values["max-restarts"]),
  };
}

// The server name the host sees; Claude Code names its tools mcp__kbl__<tool>.
const SERVER = "kbl";
const LONG_RUN_MS = 60_000;
const ROOT_PREFIX = "kbl-agents-";

// The wait after the n-th empty answer in a row (n from 0): doubles each time, up to the cap.
// Each relaunch of a real host spends model tokens to learn nothing is ready.
export function idleWait(n, baseMs, maxMs) {
  return Math.min(baseMs * 2 ** Math.min(n, 30), Math.max(maxMs, baseMs));
}

// A launcher killed with SIGKILL leaves its 0600 config dir behind. Each root records its
// owner's pid, so one whose owner is gone is stale and one still running is left alone.
function removeStaleRoots() {
  for (const name of readdirSync(tmpdir())) {
    if (!name.startsWith(ROOT_PREFIX)) continue;
    const dir = join(tmpdir(), name);
    try {
      const pid = Number(readFileSync(join(dir, "pid"), "utf8"));
      if (!Number.isInteger(pid) || pid < 1) continue;
      try {
        process.kill(pid, 0);
        continue;
      } catch (e) {
        if (e.code !== "ESRCH") continue;
      }
      rmSync(dir, { recursive: true, force: true });
    } catch {
      // Not ours, or unreadable: leave it.
    }
  }
}

// The settings kbl-mcp may be given from the launcher's environment. Tokens are never copied.
const PASSED_ENV = [
  "KBL_TENANT",
  "KBL_NAMESPACE",
  "KBL_FELIX_BROKER",
  "KBL_FELIX_BROKER_NAME",
  "KBL_FELIX_CA_FILE",
  "KBL_CLAIM_VISIBILITY_MS",
  "KBL_EXTEND_EVERY_MS",
];

function mcpConfig(options, agent) {
  const env = {};
  for (const key of PASSED_ENV) if (process.env[key] !== undefined) env[key] = process.env[key];
  Object.assign(env, { KBL_RUN: options.run, KBL_AGENT: agent.name, KBL_FELIX_TOKEN: agent.felix, KBL_TRACKER_TOKEN: agent.tracker });
  if (options.caFile) env.KBL_FELIX_CA_FILE = options.caFile;
  if (options.trackerUrl) env.KBL_TRACKER_URL = options.trackerUrl;
  return { mcpServers: { [SERVER]: { command: options.mcp, env } } };
}

function hostCommand(options, config) {
  if (options.host) return [options.host, [...options.hostArgs, LOOP_PROMPT]];
  const defaults = options.hostArgs.length
    ? options.hostArgs
    : ["--mcp-config", config, "--strict-mcp-config", "--allowedTools", `mcp__${SERVER}`];
  return ["claude", ["-p", LOOP_PROMPT, ...defaults]];
}

function hostEnv() {
  const env = { ...process.env };
  for (const key of Object.keys(env)) if (/^KBL_.*(TOKEN|AGENT)$/.test(key)) delete env[key];
  return env;
}

export async function runAgents(argv, log = console.log) {
  const options = readOptions(argv);
  removeStaleRoots();
  const root = mkdtempSync(join(tmpdir(), ROOT_PREFIX));
  writeFileSync(join(root, "pid"), String(process.pid), { mode: 0o600 });
  let stopping = false;
  let failed = false;
  const hosts = new Set();
  const wake = new Set();
  const sleep = (ms) =>
    new Promise((done) => {
      const timer = setTimeout(finish, ms);
      function finish() {
        clearTimeout(timer);
        wake.delete(finish);
        done();
      }
      wake.add(finish);
    });

  // "Active" is running, or waiting to restart after an early exit. Sessions that found the
  // queue empty keep asking while another is active, because that one may hold an item that
  // will lapse back to the group.
  const state = new Map(options.agents.map((a) => [a.name, "starting"]));
  const active = (except) => [...state].some(([name, s]) => name !== except && s !== "idle" && s !== "stopped");

  function runHost(agent, dir, config) {
    const [command, args] = hostCommand(options, config);
    return new Promise((done) => {
      let empty = false;
      const child = spawn(command, args, {
        cwd: dir,
        env: { ...hostEnv(), KBL_MCP_CONFIG: config },
        stdio: ["ignore", "pipe", "pipe"],
      });
      hosts.add(child);
      const forward = (stream, out) =>
        createInterface({ input: stream }).on("line", (line) => {
          if (line.trim() === EMPTY_MARKER) empty = true;
          if (line) out.write(`[${agent.name}] ${line}\n`);
        });
      forward(child.stdout, process.stdout);
      forward(child.stderr, process.stderr);
      child.on("error", (e) => {
        log(`${agent.name}: could not start ${command}: ${e.message}`);
        hosts.delete(child);
        done({ code: 127, empty: false });
        child.removeAllListeners("close");
      });
      // `close` comes after the output streams end, so the marker line is never missed.
      child.on("close", (code, signal) => {
        hosts.delete(child);
        done({ code: code ?? signal, empty });
      });
    });
  }

  async function session(agent) {
    const dir = join(root, agent.name);
    mkdirSync(dir, { mode: 0o700, recursive: true });
    const config = join(dir, ".mcp.json");
    writeFileSync(config, JSON.stringify(mcpConfig(options, agent), null, 2), { mode: 0o600 });
    let early = 0;
    let idle = 0;
    while (!stopping) {
      state.set(agent.name, "running");
      log(`starting ${agent.name}`);
      const startedAt = Date.now();
      const { code, empty } = await runHost(agent, dir, config);
      // A host that ran for a while did work; only quick failures in a row count.
      if (Date.now() - startedAt > LONG_RUN_MS) {
        early = 0;
        idle = 0;
      }
      if (stopping) break;
      if (empty) {
        early = 0;
        state.set(agent.name, "idle");
        const wait = idleWait(idle++, options.idleMs, options.idleMaxMs);
        log(`${agent.name} found nothing ready; asking again in ${wait / 1000}s`);
        await sleep(wait);
        if (stopping || !active(agent.name)) break;
        continue;
      }
      if (++early > options.maxRestarts) {
        log(`giving up on ${agent.name} after ${options.maxRestarts} early exits in a row`);
        failed = true;
        break;
      }
      idle = 0;
      state.set(agent.name, "restarting");
      log(`restarting ${agent.name} (exited ${code} without saying the queue was empty)`);
      await sleep(options.restartMs);
    }
    state.set(agent.name, "stopped");
  }

  function stop(why) {
    if (stopping) return;
    stopping = true;
    log(`${why}: stopping ${hosts.size} session(s)`);
    for (const h of hosts) h.kill("SIGTERM");
    for (const f of [...wake]) f();
    setTimeout(() => {
      for (const h of hosts) h.kill("SIGKILL");
    }, 10_000).unref();
  }
  process.on("SIGINT", () => stop("interrupted"));
  process.on("SIGTERM", () => stop("terminated"));

  try {
    await Promise.all(options.agents.map(session));
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
  log(stopping ? "stopped" : failed ? "stopped with a session given up on" : "the queue is empty; every session is done");
  return stopping ? 130 : failed ? 1 : 0;
}

if (import.meta.url === `file://${process.argv[1]}`) {
  runAgents(process.argv.slice(2)).then(
    (code) => process.exit(code),
    (e) => {
      console.error(e.message);
      process.exit(2);
    },
  );
}
