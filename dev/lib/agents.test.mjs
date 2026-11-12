// The launcher with a shell script for a host: no Felix, no model.
import assert from "node:assert/strict";
import { spawn, spawnSync } from "node:child_process";
import { existsSync, mkdtempSync, readFileSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { test } from "node:test";

import { idleWait } from "../agents.mjs";

const LAUNCHER = resolve("dev/agents.mjs");

function setup(agents) {
  const dir = mkdtempSync(join(tmpdir(), "agents-test-"));
  const tokens = join(dir, "tokens.json");
  writeFileSync(tokens, JSON.stringify(agents));
  return { dir, tokens };
}

const common = (tokens, script) => [
  LAUNCHER, "--run", "r1", "--tokens", tokens, "--mcp", "/bin/kbl-mcp",
  "--host", "sh", "--host-arg=-c", "--host-arg", script,
  "--restart-delay", "0", "--idle-delay", "0.5",
];

test("a session that exits without saying the queue is empty is started again", () => {
  const { dir, tokens } = setup({ a: { felix: "f", tracker: "ta" } });
  // Dies twice, then reports an empty queue; also records the config it was given.
  const script = `n=$(cat ${dir}/n 2>/dev/null || echo 0); echo $((n+1)) > ${dir}/n; cp "$KBL_MCP_CONFIG" ${dir}/cfg; [ "$n" -ge 2 ] && echo KBL_QUEUE_EMPTY; [ "$n" -ge 2 ] || exit 1`;
  const run = spawnSync("node", common(tokens, script), { encoding: "utf8" });
  assert.equal(run.status, 0, run.stderr);
  assert.equal(readFileSync(join(dir, "n"), "utf8").trim(), "3");
  assert.equal((run.stdout.match(/restarting a/g) ?? []).length, 2);
  const server = JSON.parse(readFileSync(join(dir, "cfg"), "utf8")).mcpServers.kbl;
  assert.equal(server.command, "/bin/kbl-mcp");
  assert.deepEqual(
    [server.env.KBL_RUN, server.env.KBL_AGENT, server.env.KBL_FELIX_TOKEN, server.env.KBL_TRACKER_TOKEN],
    ["r1", "a", "f", "ta"],
  );
});

test("each session gets its own agent name and tracker token", () => {
  const { dir, tokens } = setup({ a: { felix: "f1", tracker: "ta" }, b: { felix: "f2", tracker: "tb" } });
  const script = `grep -o '"KBL_TRACKER_TOKEN": "[a-z]*"' "$KBL_MCP_CONFIG" >> ${dir}/seen; echo KBL_QUEUE_EMPTY`;
  const run = spawnSync("node", common(tokens, script), { encoding: "utf8" });
  assert.equal(run.status, 0, run.stderr);
  // A session that finds nothing may be asked again while another is still going.
  assert.deepEqual([...new Set(readFileSync(join(dir, "seen"), "utf8").trim().split("\n"))].sort(), [
    '"KBL_TRACKER_TOKEN": "ta"',
    '"KBL_TRACKER_TOKEN": "tb"',
  ]);
});

test("two sessions may not share a tracker token", () => {
  const { tokens } = setup({ a: { felix: "f1", tracker: "t" }, b: { felix: "f2", tracker: "t" } });
  const run = spawnSync("node", common(tokens, "true"), { encoding: "utf8" });
  assert.equal(run.status, 2);
  assert.match(run.stderr, /share a tracker token/);
});

test("Ctrl-C stops every session and the launcher", async () => {
  const { dir, tokens } = setup({ a: { felix: "f1", tracker: "ta" }, b: { felix: "f2", tracker: "tb" } });
  const script = `echo $$ >> ${dir}/pids; exec sleep 60`;
  const child = spawn("node", common(tokens, script), { stdio: ["ignore", "pipe", "inherit"] });
  const exited = new Promise((done) => child.on("exit", (code) => done(code)));
  for (let tries = 0; tries < 100; tries++) {
    const seen = (() => { try { return readFileSync(join(dir, "pids"), "utf8").trim().split("\n").length; } catch { return 0; } })();
    if (seen === 2) break;
    await new Promise((r) => setTimeout(r, 50));
  }
  child.kill("SIGINT");
  assert.equal(await exited, 130);
  for (const pid of readFileSync(join(dir, "pids"), "utf8").trim().split("\n")) {
    assert.throws(() => process.kill(Number(pid), 0), { code: "ESRCH" }, `host ${pid} is gone`);
  }
});

test("a marker inside other words does not count, and a token may hold =", () => {
  const { dir } = setup({});
  const script = `echo "I will print KBL_QUEUE_EMPTY later"; cp "$KBL_MCP_CONFIG" ${dir}/cfg; exit 1`;
  const run = spawnSync(
    "node",
    [LAUNCHER, "--run", "r1", "--agent", "a=f==,t=", "--mcp", "/bin/kbl-mcp", "--host", "sh", "--host-arg=-c", "--host-arg", script,
      "--restart-delay", "0", "--max-restarts", "2"],
    { encoding: "utf8" },
  );
  assert.equal(run.status, 1, run.stdout);
  assert.match(run.stdout, /giving up on a after 2 early exits/);
  const env = JSON.parse(readFileSync(join(dir, "cfg"), "utf8")).mcpServers.kbl.env;
  assert.deepEqual([env.KBL_FELIX_TOKEN, env.KBL_TRACKER_TOKEN], ["f==", "t="]);
});

test("the idle wait doubles up to the cap", () => {
  assert.deepEqual([0, 1, 2, 3, 4, 5].map((n) => idleWait(n, 15_000, 300_000)), [15_000, 30_000, 60_000, 120_000, 240_000, 300_000]);
  assert.equal(idleWait(1000, 15_000, 300_000), 300_000);
  assert.equal(idleWait(3, 20_000, 10_000), 20_000);
});

test("a session that keeps finding nothing is asked again less and less often", () => {
  const { dir, tokens } = setup({ a: { felix: "f1", tracker: "ta" }, b: { felix: "f2", tracker: "tb" } });
  // a holds on for a while; b is empty at once and must wait out a.
  const script = `if [ "$(grep -o '"KBL_AGENT": "[a-z]"' "$KBL_MCP_CONFIG")" = '"KBL_AGENT": "a"' ]; then sleep 2; fi; echo KBL_QUEUE_EMPTY`;
  const args = common(tokens, script).slice(0, -2).concat(["--idle-delay", "0.1", "--idle-max", "0.4"]);
  const run = spawnSync("node", args, { encoding: "utf8" });
  assert.equal(run.status, 0, run.stderr);
  const waits = [...run.stdout.matchAll(/b found nothing ready; asking again in ([\d.]+)s/g)].map((m) => Number(m[1]));
  assert.deepEqual(waits.slice(0, 4), [0.1, 0.2, 0.4, 0.4]);
});

test("a config dir left by a killed launcher is removed on the next start", () => {
  const { tokens } = setup({ a: { felix: "f1", tracker: "ta" } });
  const stale = mkdtempSync(join(tmpdir(), "kbl-agents-"));
  writeFileSync(join(stale, "pid"), "999999999");
  const live = mkdtempSync(join(tmpdir(), "kbl-agents-"));
  writeFileSync(join(live, "pid"), String(process.pid));
  const run = spawnSync("node", common(tokens, "echo KBL_QUEUE_EMPTY"), { encoding: "utf8" });
  assert.equal(run.status, 0, run.stderr);
  assert.equal(existsSync(stale), false);
  assert.equal(existsSync(live), true);
});
