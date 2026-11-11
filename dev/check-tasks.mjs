// Runs the task tools against the development stack: a planner posts tasks and
// four workers finish them, a task that is handed back twice is not handed out
// a third time, and a worker that restarts under its name gets its task back
// at once. Each agent is its own felix-mcp process, as under an MCP host.
//
//   cargo build -p felix-mcp -p agents-tool && node dev/check-tasks.mjs
import { agent as spawnAgent, check, createRun, felixToken, giveUpAfter, killAll, sleep } from "./lib/stack.mjs";

const RUN = `tasks-${Date.now().toString(36)}`;
const TASKS = 8;

const token = await felixToken();
createRun(RUN, "task tools check");
const agent = (name, extraEnv) => spawnAgent(token, RUN, name, extraEnv);

giveUpAfter(120, "the task check");

try {
  // Four workers finish what a planner posts.
  const planner = await agent("planner");
  const workers = await Promise.all(["w1", "w2", "w3", "w4"].map((name) => agent(name)));
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

  // A claim that lapses while its worker still holds the task. Felix answers the
  // worker's extension with stale_claim, which felix-mcp must read as a lost claim.
  const slowpoke = await agent("w7", { AGENTS_CLAIM_VISIBILITY_MS: "2000", AGENTS_EXTEND_EVERY_MS: "6000" });
  await planner.tool("task_post", { title: "lapse", body: "lapses", id: "lapse" });
  const lapsing = await slowpoke.tool("task_claim", { wait_seconds: 5 });
  check(lapsing.claimed && lapsing.attempt === 1, "w7 claimed a task with a 2 second visibility");
  await sleep(3000);
  const taker = await agent("w8");
  const taken = await taker.tool("task_claim", { wait_seconds: 5 });
  check(taken.claimed && taken.task.id === "lapse" && taken.attempt === 2, "w8 was handed the lapsed task");
  await sleep(7000);
  const late = await slowpoke.tool("task_complete", { task: "lapse", result: "late but first" });
  check(
    late.status === "done" && /lapsed/.test(late.warning ?? ""),
    "w7 was told its claim lapsed, and its result still landed first",
  );
  const lateCopy = await taker.tool("task_complete", { task: "lapse", result: "second" });
  check(lateCopy.status === "duplicate", "w8's later result was a duplicate");
  planner.kill();
} finally {
  killAll();
}
