// Starts kbl-mcp the way an MCP host would, over stdio with a token in the
// environment, and checks that it lists its tools and answers one.
//
//   cargo build -p kbl-mcp && node dev/check-mcp.mjs
import { spawn } from "node:child_process";
import { createInterface } from "node:readline";

const env = (name, fallback) => process.env[name] || fallback;
const CONTROL_PLANE = env("KBL_FELIX_CONTROL_PLANE", "http://127.0.0.1:8443");
const IDP = env("KBL_IDP", "http://127.0.0.1:9400");
const BINARY = env("KBL_MCP", "target/debug/kbl-mcp");
const TENANT = "agents";

async function json(url, init) {
  const response = await fetch(url, init);
  if (!response.ok) throw new Error(`${url} -> ${response.status}: ${await response.text()}`);
  return response.json();
}

// No agent identities exist yet, so the tenant's administrator stands in for the agent.
const idToken = (await json(`${IDP}/token?sub=agents-admin&aud=felix-agents`)).id_token;
const token = (
  await json(`${CONTROL_PLANE}/v1/tenants/${TENANT}/token/exchange`, {
    method: "POST",
    headers: { "content-type": "application/json", authorization: `Bearer ${idToken}` },
    body: JSON.stringify({ audience: "felix-broker" }),
  })
).felix_token;

const child = spawn(BINARY, [], {
  env: { ...process.env, KBL_RUN: "check", KBL_AGENT: "w1", KBL_FELIX_TOKEN: token },
  stdio: ["pipe", "pipe", "inherit"],
});
child.on("error", (error) => {
  console.error(`could not start ${BINARY}: ${error.message}`);
  process.exit(1);
});
child.stdin.on("error", () => {}); // the exit handler reports why it closed
setTimeout(() => {
  console.error("kbl-mcp did not finish the check in 30 seconds");
  child.kill();
  process.exit(1);
}, 30_000).unref();
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

try {
  const init = await call("initialize", {
    protocolVersion: "2025-06-18",
    capabilities: {},
    clientInfo: { name: "check-mcp", version: "0" },
  });
  send({ method: "notifications/initialized" });
  console.log(`ok   initialized (${init.serverInfo.name} ${init.serverInfo.version}, protocol ${init.protocolVersion})`);

  const names = (await call("tools/list", {})).tools.map((tool) => tool.name);
  if (!names.includes("run_info")) throw new Error(`tools/list has ${JSON.stringify(names)}`);
  console.log(`ok   tools/list: ${names.join(", ")}`);

  const result = await call("tools/call", { name: "run_info", arguments: {} });
  if (result.isError) throw new Error(`run_info failed: ${result.content[0]?.text}`);
  const info = JSON.parse(result.content[0].text);
  if (info.run !== "check" || info.agent !== "w1" || info.tenant !== TENANT) {
    throw new Error(`run_info said ${JSON.stringify(info)}`);
  }
  console.log(`ok   run_info: ${JSON.stringify(info)}`);
} finally {
  child.kill();
}
