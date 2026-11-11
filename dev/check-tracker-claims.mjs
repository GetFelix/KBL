// Runs the tracker's claim path against the dev stack, with each agent a
// felix-mcp process that calls the tracker over REST:
//   1. two agents race work_next for six items: each item is claimed once;
//   2. an agent is killed holding an item: it goes to another agent after the
//      visibility, with a lapse record in between;
//   3. the tracker dies between polling a delivery and logging its claim: after a
//      restart the item is not held by anyone, and is claimed and logged once.
// An item is done only after its done record is written, so a finished item is
// never handed out again.
//
//   cargo build -p felix-mcp -p agents-tool -p agents-tracker && node dev/check-tracker-claims.mjs
import {
  agent as spawnAgent,
  check,
  felixToken,
  giveUpAfter,
  killAll,
  personRecord,
  sleep,
  startTracker,
  TRACKER_URL,
  workLog,
} from "./lib/stack.mjs";

const RUN = "tracker-check";
const tokens = {
  agents: { "tok-w1": "w1", "tok-w2": "w2", "tok-w3": "w3" },
  people: { "tok-gab": "gab" },
};
// Claims lapse after 4 s here; the agents extend every second.
const timing = { AGENTS_CLAIM_VISIBILITY_MS: "4000", AGENTS_EXTEND_EVERY_MS: "1000" };

giveUpAfter(240, "the tracker claim check");

const felix = await felixToken();
const agent = (name) =>
  spawnAgent(felix, RUN, name, {
    ...timing,
    AGENTS_TRACKER_URL: TRACKER_URL,
    AGENTS_TRACKER_TOKEN: `tok-${name}`,
  });

async function seed(id, tier) {
  await personRecord("tok-gab", { kind: "imported", item: id, source: "check", upstream_id: id, title: `item ${id}` });
  await personRecord("tok-gab", { kind: "accepted", item: id, tier });
}

const kindsOf = (item) => workLog().filter((r) => r.item === item).map((r) => r.kind);
const claimsOf = (item) => workLog().filter((r) => r.item === item && r.kind === "claimed");

// Asks for work until `wait` seconds have passed without any.
async function takeNext(who, seconds) {
  const until = Date.now() + seconds * 1000;
  for (;;) {
    const reply = await who.tool("work_next", { wait_seconds: 3 });
    if (reply.claimed || Date.now() > until) return reply;
  }
}

let tracker = await startTracker(tokens);
try {
  // 1. A race for six items.
  const ids = ["r1", "r2", "r3", "r4", "r5", "r6"];
  for (const [n, id] of ids.entries()) await seed(id, 1 + (n % 3));
  const [w1, w2] = await Promise.all([agent("w1"), agent("w2")]);
  const finished = [];
  await Promise.all(
    [w1, w2].map(async (who) => {
      for (;;) {
        const reply = await who.tool("work_next", { wait_seconds: 2 });
        if (!reply.claimed) return;
        await who.tool("work_complete", { item: reply.item.id, cost_micros: 1000 });
        finished.push({ item: reply.item.id, by: who.name });
      }
    }),
  );
  check(
    finished.length === ids.length && new Set(finished.map((f) => f.item)).size === ids.length,
    "two agents racing finished six items, each once",
  );
  for (const id of ids) {
    const claims = claimsOf(id);
    const done = workLog().filter((r) => r.item === id && r.kind === "done");
    check(
      claims.length === 1 && done.length === 1 && claims[0].by === done[0].by && claims[0].actor === "agent",
      `${id} has one claimed record and one done record, from the same agent`,
    );
  }

  // 2. An agent dies holding an item.
  await seed("dead1", 0);
  const w3 = await agent("w3");
  const held = await w3.tool("work_next", { wait_seconds: 5 });
  check(held.claimed && held.item.id === "dead1", "w3 claimed dead1");
  w3.kill();
  const taken = await takeNext(w1, 30);
  check(taken.claimed && taken.item.id === "dead1" && taken.attempt === 1, "w1 was handed dead1 after w3 stopped extending");
  check(
    JSON.stringify(workLog().filter((r) => r.item === "dead1").map((r) => `${r.kind}:${r.by}`)) ===
      JSON.stringify(["imported:gab", "accepted:gab", "claimed:w3", "released:tracker", "claimed:w1"]),
    "dead1 was claimed by w3, released as lapsed by the tracker, then claimed by w1",
  );
  await w1.tool("work_complete", { item: "dead1" });

  // 3. The tracker dies between the poll and the log.
  tracker.kill();
  tracker = await startTracker(tokens, { AGENTS_TRACKER_CRASH_AFTER_POLL: "true" });
  await seed("crash1", 0);
  let died = false;
  await w1.tool("work_next", { wait_seconds: 5 }).catch(() => (died = true));
  await tracker.exited;
  check(died, "w1's work_next failed when the tracker died after polling");
  check(claimsOf("crash1").length === 0, "no claimed record was written for crash1");
  tracker = await startTracker(tokens);
  const again = await takeNext(w2, 30);
  check(again.claimed && again.item.id === "crash1", "after the restart w2 was handed crash1");
  check(claimsOf("crash1").length === 1 && claimsOf("crash1")[0].by === "w2", "crash1 was logged claimed once, by w2");
  await w2.tool("work_complete", { item: "crash1" });

  // The stranded delivery lapses and must not claim a finished item again.
  await sleep(6000);
  const nothing = await w1.tool("work_next", { wait_seconds: 3 });
  check(!nothing.claimed, "a finished item is not handed out again");
  check(kindsOf("crash1").filter((k) => k === "claimed").length === 1, "crash1 still has one claimed record");
} finally {
  killAll();
}
