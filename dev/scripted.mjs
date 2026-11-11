// Runs a job with scripted agents against the dev stack. The seed decides the
// tasks and what happens to each, so a seed always gives the same run.
//
//   cargo build -p felix-mcp -p agents-tool
//   node dev/scripted.mjs --seed 7 --workers 3 [--run name]
import { action, expected, outcomes, plan, sameEnds } from "./lib/script.mjs";
import { agent, check, createRun, felixToken, giveUpAfter, killAll, newRunId, runLog } from "./lib/stack.mjs";

export async function scriptedPlanner(planner, tasks) {
  for (const { id, title, body } of tasks) await planner.tool("task_post", { id, title, body });
}

// Takes tasks, doing with each what its fate says, until the log shows every
// task settled. An empty claim is not the end: a handed-back or lapsed task
// can take longer than one wait to come round again.
export async function scriptedWorker(worker, tasks, seed, run) {
  const fates = new Map(tasks.map((t) => [t.id, t.fate]));
  for (;;) {
    const claim = await worker.tool("task_claim", { wait_seconds: 3 });
    if (!claim.claimed) {
      const ends = outcomes(runLog(run));
      if (tasks.every((t) => ends[t.id])) return;
      continue;
    }
    const id = claim.task.id;
    switch (action(fates.get(id), claim.attempt)) {
      case "complete":
        await worker.tool("task_complete", { task: id, result: `${worker.name} reviewed ${claim.task.body}` });
        break;
      case "release":
        await worker.tool("task_release", { task: id, reason: `seed ${seed}: cannot do it` });
        break;
      default:
        await worker.tool("task_give_up", { task: id, reason: "given up" });
    }
  }
}

// One whole run. Returns how each task ended, from the log.
export async function scriptedRun({ seed, workers, run = newRunId(`scripted-${seed}`), token }) {
  const tasks = plan(seed);
  createRun(run, `scripted run, seed ${seed}`);
  const planner = await agent(token, run, "planner");
  const crew = await Promise.all(Array.from({ length: workers }, (_, n) => agent(token, run, `w${n + 1}`)));
  await scriptedPlanner(planner, tasks);
  await Promise.all(crew.map((w) => scriptedWorker(w, tasks, seed, run)));
  return { tasks, run, ends: outcomes(runLog(run)) };
}

if (import.meta.url === `file://${process.argv[1]}`) {
  const args = Object.fromEntries(process.argv.slice(2).reduce((a, v, i, all) => (i % 2 ? a : [...a, [v.slice(2), all[i + 1]]]), []));
  const seed = Number(args.seed ?? 1);
  giveUpAfter(180, "the scripted run");
  try {
    const { tasks, run, ends } = await scriptedRun({
      seed,
      workers: Number(args.workers ?? 3),
      run: args.run,
      token: await felixToken(),
    });
    check(sameEnds(ends, expected(tasks)), `run ${run}: all ${tasks.length} tasks ended as seed ${seed} says`);
  } finally {
    killAll();
  }
}
