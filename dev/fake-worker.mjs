// A scripted worker for supervise tests and CI, so no vendor is needed. It
// reads the MCP config the supervisor rendered for it, starts the kbl server
// that config names, completes every item work_next hands it, and exits 0
// when two waits in a row hand it nothing.
//
//   node dev/fake-worker.mjs <mcp_config_path>
//
// FAKE_WORKER_EXIT makes it exit with that code after its first item instead,
// to stand in for a worker that fails.
import { spawn } from "node:child_process";
import { readFileSync, statSync } from "node:fs";
import { createInterface } from "node:readline";

const path = process.argv[2];
if (!path) {
  console.error("usage: fake-worker.mjs <mcp_config_path>");
  process.exit(2);
}
// The config holds the worker's token: anything but mode 600 is a bug.
const mode = statSync(path).mode & 0o777;
if (mode !== 0o600) {
  console.error(`${path} is mode ${mode.toString(8)}, not 600`);
  process.exit(3);
}
const server = JSON.parse(readFileSync(path, "utf8")).mcpServers.kbl;
const name = server.env.KBL_AGENT;
const child = spawn(server.command, server.args ?? [], {
  env: { PATH: process.env.PATH ?? "", ...server.env },
  stdio: ["pipe", "pipe", "inherit"],
});
child.stdin.on("error", () => {});
const lines = createInterface({ input: child.stdout })[Symbol.asyncIterator]();
const exited = new Promise((resolve) => child.on("exit", (code) => resolve(`kbl-mcp exited with ${code}`)));

let nextId = 1;
const send = (message) => child.stdin.write(`${JSON.stringify({ jsonrpc: "2.0", ...message })}\n`);
async function call(method, params) {
  const id = nextId++;
  send({ id, method, params });
  for (;;) {
    const next = await Promise.race([lines.next(), exited.then((why) => Promise.reject(new Error(why)))]);
    if (next.done) throw new Error("kbl-mcp closed its output");
    const reply = JSON.parse(next.value);
    if (reply.id !== id) continue;
    if (reply.error) throw new Error(`${method}: ${reply.error.message}`);
    return reply.result;
  }
}
async function tool(tool, args) {
  const result = await call("tools/call", { name: tool, arguments: args });
  const text = result.content[0]?.text ?? "";
  if (result.isError) throw new Error(`${tool}: ${text}`);
  return JSON.parse(text);
}

try {
  await call("initialize", { protocolVersion: "2025-06-18", capabilities: {}, clientInfo: { name: "fake-worker", version: "0" } });
  send({ method: "notifications/initialized" });
  let empty = 0;
  while (empty < 2) {
    const reply = await tool("work_next", { wait_seconds: 3 });
    if (!reply.claimed) {
      empty++;
      continue;
    }
    empty = 0;
    await tool("work_complete", { item: reply.item.id });
    console.error(`fake-worker ${name}: completed ${reply.item.id}`);
    if (process.env.FAKE_WORKER_EXIT) process.exit(Number(process.env.FAKE_WORKER_EXIT));
  }
  child.kill();
  process.exit(0);
} catch (error) {
  console.error(`fake-worker ${name}: ${error.message}`);
  child.kill();
  process.exit(1);
}
