// Runs the task tools against the development stack: a planner posts tasks and
// four workers finish them, a task that is handed back twice is not handed out
// a third time, and a worker that restarts under its name gets its task back
// at once. Each agent is its own felix-mcp process, as under an MCP host.
//
//   cargo build -p felix-mcp -p agents-tool && node dev/check-tasks.mjs
import { spawn, spawnSync } from "node:child_process";
import { createInterface } from "node:readline";

const env = (name, fallback) => process.env[name] || fallback;
const CONTROL_PLANE = env("AGENTS_FELIX_CONTROL_PLANE", "http://127.0.0.1:8443");
const IDP = env("AGENTS_IDP", "http://127.0.0.1:9400");
const MCP = env("FELIX_MCP", "target/debug/felix-mcp");
const TOOL = env("AGENTS_TOOL", "target/debug/agents-tool");
const TENANT = "agents";
const RUN = `tasks-${Date.now().toString(36)}`;
const TASKS = 8;

async function json(url, init) {
  const response = await fetch(url, init);
  if (!response.ok) throw new Error(`${url} -> ${response.status}: ${await response.text()}`);
  return response.json();
}

// No agent identities exist yet, so the tenant's administrator stands in for every agent.
const idToken = (await json(`${IDP}/token?sub=agents-admin&aud=felix-agents`)).id_token;
const token = (
  await json(`${CONTROL_PLANE}/v1/tenants/${TENANT}/token/exchange`, {
    method: "POST",
    headers: { "content-type": "application/json", authorization: `Bearer ${idToken}` },
    body: JSON.stringify({ audience: "felix-broker" }),
  })
).felix_token;

const created = spawnSync(TOOL, ["run", "create", RUN, "--goal", "task tools check", "--budget-usd", "1"], {
  stdio: "inherit",
});
if (created.status !== 0) throw new Error("could not create the run");

const running = new Set();

// One felix-mcp process, spoken to over stdio like an MCP host would.
async function agent(name) {
  const child = spawn(MCP, [], {
    env: { ...process.env, AGENTS_RUN: RUN, AGENTS_AGENT: name, AGENTS_FELIX_TOKEN: token },
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
    clientInfo: { name: "check-tasks", version: "0" },
  });
  send({ method: "notifications/initialized" });
  return {
    // The tool's JSON reply, or a thrown error carrying the text the model would see.
    async tool(tool, args) {
      const result = await call("tools/call", { name: tool, arguments: args });
      const text = result.content[0]?.text ?? "";
      if (result.isError) throw new Error(`${name} ${tool}: ${text}`);
      return JSON.parse(text);
    },
    kill() {
      child.kill();
      running.delete(child);
    },
  };
}

function check(ok, what) {
  if (!ok) throw new Error(`failed: ${what}`);
  console.log(`ok   ${what}`);
}

setTimeout(() => {
  console.error("the task check did not finish in 120 seconds");
  for (const child of running) child.kill();
  process.exit(1);
}, 120_000).unref();

try {
  // Four workers finish what a planner posts.
  const planner = await agent("planner");
  const workers = await Promise.all(["w1", "w2", "w3", "w4"].map(agent));
  const posted = [];
  for (let n = 1; n <= TASKS; n++) {
    const reply = await planner.tool("task_post", { title: `review file ${n}`, body: `file ${n}` });
    posted.push(reply.task);
  }
  check(posted.length === TASKS, `the planner posted ${TASKS} tasks`);

  const done = [];
  await Promise.all(
    workers.map(async (worker, n) => {
      for (;;) {
        const claim = await worker.tool("task_claim", { wait_seconds: 3 });
        if (!claim.claimed) return;
        const complete = await worker.tool("task_complete", {
          task: claim.task.id,
          result: `w${n + 1} reviewed ${claim.task.body}`,
        });
        done.push({ task: claim.task.id, status: complete.status });
      }
    }),
  );
  check(
    done.length === TASKS && done.every((d) => d.status === "done") && new Set(done.map((d) => d.task)).size === TASKS,
    `four workers finished all ${TASKS} tasks, each once`,
  );
  check(
    posted.every((id) => done.some((d) => d.task === id)),
    "every posted task has a result",
  );
  for (const w of workers) w.kill();

  // A task handed back twice is dead-lettered on its third delivery.
  const [flaky] = await Promise.all([agent("w5")]);
  await planner.tool("task_post", { title: "poison", body: "never works", id: "poison" });
  for (const attempt of [1, 2]) {
    const claim = await flaky.tool("task_claim", { wait_seconds: 5 });
    check(claim.claimed && claim.task.id === "poison" && claim.attempt === attempt, `poison delivered, attempt ${attempt}`);
    await flaky.tool("task_release", { task: "poison", reason: "cannot do it" });
  }
  const third = await flaky.tool("task_claim", { wait_seconds: 3 });
  check(!third.claimed, "the third delivery is not handed to the worker");
  flaky.kill();

  // A worker that dies holding a task gets it back, under its name, without waiting out the claim.
  await planner.tool("task_post", { title: "slow", body: "takes a while", id: "slow" });
  const first = await agent("w6");
  const held = await first.tool("task_claim", { wait_seconds: 5 });
  check(held.claimed && held.task.id === "slow", "w6 claimed the slow task");
  first.kill();
  const started = Date.now();
  const second = await agent("w6");
  const again = await second.tool("task_claim", { wait_seconds: 5 });
  check(again.claimed && again.task.id === "slow", "w6, restarted, got the slow task back");
  check(Date.now() - started < 30_000, "without waiting for the claim to lapse");
  await second.tool("task_complete", { task: "slow", result: "finished" });
  second.kill();
  planner.kill();
} finally {
  for (const child of running) child.kill();
}
