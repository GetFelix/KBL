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
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
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
  --idle-delay <s>      wait before asking again after a session found nothing (default: 15)

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
      help: { type: "boolean", short: "h" },
    },
  });
  if (values.help) throw new Error(USAGE);
  if (!values.run) throw new Error(`--run is required\n${USAGE}`);
  const agents = values.tokens ? JSON.parse(readFileSync(values.tokens, "utf8")) : {};
  for (const spec of values.agent ?? []) {
    const [name, rest = ""] = spec.split("=");
    const [felix, tracker] = rest.split(",");
    if (!name || !felix || !tracker) throw new Error(`--agent wants <name>=<felix token>,<tracker token>, got "${name}"`);
    agents[name] = { felix, tracker };
  }
  let names = Object.keys(agents);
  if (values.sessions) names = names.slice(0, Number(values.sessions));
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
  };
}

// The server name the host sees; Claude Code names its tools mcp__kbl__<tool>.
const SERVER = "kbl";

function mcpConfig(options, agent) {
  const env = {};
  for (const [key, value] of Object.entries(process.env)) {
    if (key.startsWith("KBL_") && !/TOKEN|AGENT$/.test(key)) env[key] = value;
  }
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

export async function runAgents(argv, log = console.log) {
  const options = readOptions(argv);
  const root = mkdtempSync(join(tmpdir(), "kbl-agents-"));
  let stopping = false;
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
        env: { ...process.env, KBL_MCP_CONFIG: config },
        stdio: ["ignore", "pipe", "pipe"],
      });
      hosts.add(child);
      const forward = (out) => (chunk) => {
        for (const line of String(chunk).split("\n")) {
          if (line.includes(EMPTY_MARKER)) empty = true;
          if (line) out.write(`[${agent.name}] ${line}\n`);
        }
      };
      child.stdout.on("data", forward(process.stdout));
      child.stderr.on("data", forward(process.stderr));
      child.on("error", (e) => {
        log(`${agent.name}: could not start ${command}: ${e.message}`);
        hosts.delete(child);
        done({ code: 127, empty: false });
      });
      child.on("exit", (code, signal) => {
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
    while (!stopping) {
      state.set(agent.name, "running");
      log(`starting ${agent.name}`);
      const { code, empty } = await runHost(agent, dir, config);
      if (stopping) break;
      if (empty) {
        state.set(agent.name, "idle");
        log(`${agent.name} found nothing ready`);
        await sleep(options.idleMs);
        if (stopping || !active(agent.name)) break;
        continue;
      }
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
  log(stopping ? "stopped" : "the queue is empty; every session is done");
  return stopping ? 130 : 0;
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
