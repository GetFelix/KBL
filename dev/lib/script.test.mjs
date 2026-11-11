// Run with `node --test dev/lib`. No Felix needed.
import assert from "node:assert/strict";
import { test } from "node:test";
import { action, expected, outcomes, plan, sameEnds } from "./script.mjs";

test("a seed always plans the same job", () => {
  assert.deepEqual(plan(7), plan(7));
  assert.notDeepEqual(plan(7), plan(8));
});

test("every fate shows up across some seeds, and each plan has between 6 and 10 tasks", () => {
  const seen = new Set();
  for (let seed = 1; seed <= 50; seed++) {
    const tasks = plan(seed);
    assert.ok(tasks.length >= 6 && tasks.length <= 10);
    tasks.forEach((t) => seen.add(t.fate));
  }
  assert.deepEqual([...seen].sort(), ["flaky", "hopeless", "ok", "poison"]);
});

test("a worker's action follows the fate and the delivery", () => {
  assert.equal(action("ok", 1), "complete");
  assert.equal(action("flaky", 1), "release");
  assert.equal(action("flaky", 2), "complete");
  assert.equal(action("poison", 2), "release");
  assert.equal(action("hopeless", 1), "give_up");
});

test("the promised ends match what the log would show", () => {
  const tasks = [
    { id: "a", fate: "ok" },
    { id: "b", fate: "flaky" },
    { id: "c", fate: "poison" },
    { id: "d", fate: "hopeless" },
  ];
  const log = [
    { kind: "task_done", task: "a", attempt: 1 },
    { kind: "task_released", task: "b", attempt: 1 },
    { kind: "task_done", task: "b", attempt: 2 },
    { kind: "task_dead", task: "c", attempt: 3, reason: "attempts used up after 2" },
    { kind: "task_dead", task: "d", attempt: 1, reason: "given up" },
  ];
  assert.ok(sameEnds(outcomes(log), expected(tasks)));
  assert.ok(!sameEnds(outcomes(log.slice(0, 4)), expected(tasks)));
  log[3].attempt = 2;
  assert.ok(!sameEnds(outcomes(log), expected(tasks)));
});
