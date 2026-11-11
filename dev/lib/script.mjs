// The scripted planner and workers: stand-ins that make the tool calls a model
// would, decided by a seed alone. The same seed posts the same tasks and gives
// each the same fate, whichever worker takes it, so a run can be repeated.

// A small seeded generator (mulberry32).
export function random(seed) {
  let state = seed >>> 0;
  return () => {
    state = (state + 0x6d2b79f5) >>> 0;
    let t = state;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

// What happens to a task: `ok` is finished at once, `flaky` is handed back on
// its first delivery and finished on its second, `poison` is handed back every
// time, and `hopeless` is given up on sight.
const FATES = [
  ["ok", 0.6],
  ["flaky", 0.15],
  ["poison", 0.1],
  ["hopeless", 0.15],
];

export function plan(seed) {
  const next = random(seed);
  const count = 6 + Math.floor(next() * 5);
  const tasks = [];
  for (let n = 1; n <= count; n++) {
    const roll = next();
    let sum = 0;
    const fate = FATES.find(([, share]) => (sum += share) > roll)?.[0] ?? "ok";
    tasks.push({ id: `f${n}`, title: `review file ${n}`, body: `file ${n}`, fate });
  }
  return tasks;
}

// What a worker does with a task on a delivery.
export function action(fate, attempt) {
  if (fate === "hopeless") return "give_up";
  if (fate === "poison" || (fate === "flaky" && attempt === 1)) return "release";
  return "complete";
}

// How each task ends, given the worker's attempt limit.
export function expected(tasks, attemptsAllowed = 2) {
  const ends = {};
  for (const { id, fate } of tasks) {
    if (fate === "hopeless") ends[id] = { status: "dead", attempt: 1, reason: "given up" };
    else if (fate === "poison") ends[id] = { status: "dead", attempt: attemptsAllowed + 1, reason: "attempts used up" };
    else ends[id] = { status: "done", attempt: fate === "flaky" ? 2 : 1 };
  }
  return ends;
}

// How each task ended, read off the run log.
export function outcomes(records) {
  const ends = {};
  for (const r of records) {
    if (r.kind === "task_done") ends[r.task] = { status: "done", attempt: r.attempt };
    else if (r.kind === "task_dead") {
      ends[r.task] = { status: "dead", attempt: r.attempt, reason: r.reason };
    }
  }
  return ends;
}

// Whether `got` is what `want` says, treating a reason as a prefix of Felix's text.
export function sameEnds(got, want) {
  const ids = new Set([...Object.keys(got), ...Object.keys(want)]);
  return [...ids].every((id) => {
    const a = got[id];
    const b = want[id];
    return a && b && a.status === b.status && a.attempt === b.attempt && (!b.reason || a.reason?.startsWith(b.reason));
  });
}
