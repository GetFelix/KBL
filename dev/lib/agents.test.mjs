// The launcher with a shell script for a host: no Felix, no model.
import assert from "node:assert/strict";
import { spawn, spawnSync } from "node:child_process";
import { mkdtempSync, readFileSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { test } from "node:test";

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
