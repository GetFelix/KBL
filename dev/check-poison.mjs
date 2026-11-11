// A task that always fails is dead-lettered after the worker's attempt limit,
// and one a worker gives up is dead-lettered at once. Both show as task_dead.
//
//   cargo build -p felix-mcp -p agents-tool && node dev/check-poison.mjs
import { agent as spawnAgent, check, createRun, felixToken, giveUpAfter, killAll, newRunId, runLog } from "./lib/stack.mjs";
import { scriptedPlanner, scriptedWorker } from "./scripted.mjs";

const RUN = newRunId("poison");
const token = await felixToken();
createRun(RUN, "poison task check");

giveUpAfter(120, "the poison task check");
try {
  const tasks = [
    { id: "poison", title: "always fails", body: "x", fate: "poison" },
    { id: "hopeless", title: "cannot be done", body: "x", fate: "hopeless" },
    { id: "fine", title: "works", body: "x", fate: "ok" },
  ];
  await scriptedPlanner(await spawnAgent(token, RUN, "planner"), tasks);
  await scriptedWorker(await spawnAgent(token, RUN, "w1"), tasks, 0, RUN);

  const log = runLog(RUN);
  const of = (task, kind) => log.filter((r) => r.task === task && r.kind === kind);

  check(of("poison", "task_released").length === 2, "the always-failing task was handed back on each of its two attempts");
  const dead = of("poison", "task_dead");
  check(dead.length === 1 && dead[0].attempt === 3, "it is task_dead on its third delivery, once");
  check(/attempts used up/.test(dead[0].reason), "and the reason says the attempts ran out");
  check(of("poison", "task_done").length === 0, "it has no result");

  const given = of("hopeless", "task_dead");
  check(given.length === 1 && given[0].attempt === 1 && given[0].reason === "given up", "a task given up is task_dead on its first delivery");
  check(of("hopeless", "task_claimed").length === 1, "and was claimed only once");

  check(of("fine", "task_done").length === 1, "a healthy task beside them still finished");

  const other = await spawnAgent(token, RUN, "w2");
  const left = await other.tool("task_claim", { wait_seconds: 3 });
  check(!left.claimed, "no dead task is handed out again");
} finally {
  killAll();
}
