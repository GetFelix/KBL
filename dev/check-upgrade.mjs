// Runs the previous build of the tracker and this one over the same
// work.events, in the order docs/upgrades.md gives, with agents that stay on
// the previous kbl-mcp throughout (clients upgrade after the tracker):
//   1. the previous tracker takes work in and hands an item to w1;
//   2. this build takes over: w1 still holds its item and finishes it, and w2
//      claims the next one;
//   3. the previous build takes over again, which is a rollback before the
//      format is raised: w2 finishes there and w1 takes the last item;
//   4. this build comes back and finishes the log's story.
// Every record either build wrote is of the first format, each item was
// claimed and done once, and neither build refused a record of the other.
// Both builds write format 1 only, so this does not show a reader stopping at
// a newer format: the unit tests of core, the tracker and the page do.
//
//   KBL_TRACKER_OLD and KBL_MCP_OLD name the previous build's binaries.
//   cargo build -p kbl-mcp -p kbl-tool -p kbl-tracker && node dev/check-upgrade.mjs
import {
  agent as spawnAgent,
  check,
  createRun,
  felixToken,
  giveUpAfter,
  killAll,
  newRunId,
  personRecord,
  startTracker,
  TRACKER_URL,
  workLog,
} from "./lib/stack.mjs";

const OLD_TRACKER = process.env.KBL_TRACKER_OLD;
const OLD_MCP = process.env.KBL_MCP_OLD;
if (!OLD_TRACKER || !OLD_MCP) {
  console.error("set KBL_TRACKER_OLD and KBL_MCP_OLD to the previous build's kbl-tracker and kbl-mcp");
  process.exit(2);
}

const RUN = newRunId("upgrade");
const tokens = { agents: { "tok-w1": "w1", "tok-w2": "w2" }, people: { "tok-gab": "gab" } };
// Long enough that a claim outlives the seconds no tracker is up.
const timing = { KBL_CLAIM_VISIBILITY_MS: "60000", KBL_EXTEND_EVERY_MS: "1000" };
// Other checks share the log, so these items are named for this run.
const tag = Date.now().toString(36);
const [a, b, c] = [`up-${tag}-a`, `up-${tag}-b`, `up-${tag}-c`];

giveUpAfter(240, "the upgrade check");

const felix = await felixToken();
createRun(RUN, "upgrade check");
const agent = (name) =>
  spawnAgent(felix, RUN, name, { ...timing, KBL_TRACKER_URL: TRACKER_URL, KBL_TRACKER_TOKEN: `tok-${name}` }, OLD_MCP);

async function seed(id) {
  await personRecord("tok-gab", { kind: "imported", item: id, source: "check", upstream_id: id, title: `item ${id}` });
  await personRecord("tok-gab", { kind: "accepted", item: id, tier: 0 });
}

// Claims until `who` holds `id`, handing back anything an earlier check left ready.
async function take(who, id) {
  for (let tries = 0; tries < 20; tries++) {
    const reply = await who.tool("work_next", { wait_seconds: 3 });
    if (!reply.claimed) continue;
    if (reply.item.id === id) return reply;
    await who.tool("work_release", { item: reply.item.id, reason: "not this check's item" });
  }
  throw new Error(`${who.name} was never handed ${id}`);
}

const mine = () => workLog().filter((r) => [a, b, c].includes(r.item));
const kindsOf = (id) => mine().filter((r) => r.item === id).map((r) => `${r.kind}:${r.by}`).join(" ");

let tracker = await startTracker(tokens, timing, OLD_TRACKER);
const swap = async (bin) => {
  tracker.kill();
  await tracker.exited;
  tracker = await startTracker(tokens, timing, bin);
};
try {
  const [w1, w2] = await Promise.all([agent("w1"), agent("w2")]);

  // 1. The previous build.
  for (const id of [a, b, c]) await seed(id);
  await take(w1, a);
  await w1.tool("work_progress", { item: a, note: "started on the previous build" });
  check(kindsOf(a).endsWith("claimed:w1 progress:w1"), "the previous tracker handed w1 its item and logged its progress");

  // 2. This build takes over the same log.
  await swap(undefined);
  await w1.tool("work_progress", { item: a, note: "carried on after the upgrade" });
  await w1.tool("work_complete", { item: a });
  check(kindsOf(a).endsWith(`done:w1`), "w1 kept its claim across the upgrade and finished on this build");
  await take(w2, b);
  check(kindsOf(b).endsWith("accepted:gab claimed:w2"), "this build handed out work the previous build took in");

  // 3. Back to the previous build: nothing this build wrote is beyond it.
  await swap(OLD_TRACKER);
  await w2.tool("work_complete", { item: b });
  check(kindsOf(b).endsWith(`done:w2`), "w2 finished on the previous build what it claimed on this one");
  await take(w1, c);
  check(kindsOf(c).endsWith("accepted:gab claimed:w1"), "the previous build handed out the last item after the rollback");

  // 4. Forward again.
  await swap(undefined);
  await w1.tool("work_complete", { item: c });

  const records = mine();
  check(records.every((r) => r.v === undefined), "every record either build wrote is of the first format");
  for (const id of [a, b, c]) {
    const of = records.filter((r) => r.item === id);
    check(
      of.filter((r) => r.kind === "claimed").length === 1 && of.filter((r) => r.kind === "done").length === 1,
      `${id} was claimed once and done once across four trackers (${kindsOf(id)})`,
    );
  }
  check(!records.some((r) => r.kind === "refused"), "neither build refused the other's records");
} finally {
  killAll();
}
