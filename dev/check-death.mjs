// A worker dies holding a task: another worker finishes it, a restart under the
// same name takes it back at once, and two results for one task are recorded
// once as the task's result and once as a duplicate.
//
//   cargo build -p kbl-mcp -p kbl-tool && node dev/check-death.mjs
import { agent as spawnAgent, check, createRun, felixToken, giveUpAfter, killAll, newRunId, runLog } from "./lib/stack.mjs";

const RUN = newRunId("death");
const token = await felixToken();
createRun(RUN, "worker death check");
// A short claim and no extensions, so a dead or stalled worker's task comes back in seconds.
const quick = { KBL_CLAIM_VISIBILITY_MS: "2000", KBL_EXTEND_EVERY_MS: "60000" };
const agent = (name, extraEnv) => spawnAgent(token, RUN, name, extraEnv);
const kinds = (task) => runLog(RUN).filter((r) => r.task === task);

giveUpAfter(120, "the worker death check");
try {
  const planner = await agent("planner");

  // A killed worker's task is finished by another worker.
  await planner.tool("task_post", { id: "orphan", title: "orphan", body: "x" });
  const doomed = await agent("w1", quick);
  const held = await doomed.tool("task_claim", { wait_seconds: 5 });
  check(held.claimed && held.task.id === "orphan" && held.attempt === 1, "w1 claimed the task");
  doomed.kill();
  const rescuer = await agent("w2", quick);
  const taken = await rescuer.tool("task_claim", { wait_seconds: 15 });
  check(taken.claimed && taken.task.id === "orphan" && taken.attempt === 2, "w2 was handed the task after w1 died");
  const finished = await rescuer.tool("task_complete", { task: "orphan", result: "w2 did it" });
  check(finished.status === "done", "w2's result is the task's result");
  const orphan = kinds("orphan");
  check(
    orphan.filter((r) => r.kind === "task_done" && r.by === "w2" && r.attempt === 2).length === 1 &&
      orphan.filter((r) => r.kind === "task_duplicate").length === 0,
    "the log has one task_done, by w2 on the second attempt",
  );

  // A worker restarted under its name takes its task back without waiting out the claim.
  await planner.tool("task_post", { id: "mine", title: "mine", body: "x" });
  const before = await agent("w3");
  const mine = await before.tool("task_claim", { wait_seconds: 5 });
  check(mine.claimed && mine.task.id === "mine" && mine.attempt === 1, "w3 claimed the task under a 90 second claim");
  before.kill();
  const started = Date.now();
  const after = await agent("w3");
  const back = await after.tool("task_claim", { wait_seconds: 10 });
  check(back.claimed && back.task.id === "mine" && back.attempt === 2, "w3, restarted, got the task back");
  check(Date.now() - started < 30_000, "without waiting for the 90 second claim to lapse");
  await after.tool("task_complete", { task: "mine", result: "w3 did it" });

  // A task finished twice has one result and one duplicate.
  await planner.tool("task_post", { id: "twice", title: "twice", body: "x" });
  const slow = await agent("w4", quick);
  const first = await slow.tool("task_claim", { wait_seconds: 5 });
  check(first.claimed && first.task.id === "twice", "w4 claimed the task");
  const second = await agent("w5", quick);
  const again = await second.tool("task_claim", { wait_seconds: 10 });
  check(again.claimed && again.task.id === "twice" && again.attempt === 2, "w5 was handed the task once w4's claim lapsed");
  const winner = await second.tool("task_complete", { task: "twice", result: "w5 first" });
  const loser = await slow.tool("task_complete", { task: "twice", result: "w4 second" });
  check(winner.status === "done" && loser.status === "duplicate", "the first result won and the second was a duplicate");
  const twice = kinds("twice");
  check(
    twice.filter((r) => r.kind === "task_done").length === 1 && twice.filter((r) => r.kind === "task_duplicate").length === 1,
    "the log has one task_done and one task_duplicate",
  );
  check(twice.find((r) => r.kind === "task_done").by === "w5", "the task_done is w5's");
} finally {
  killAll();
}
