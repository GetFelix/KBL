// Runs triage as work against the dev stack. Each root is admitted the way
// ingest admits one (imported, accepted at a tier, one triage item), and a
// scripted agent, a kbl-mcp process with no model behind it, answers each
// triage item from a fixed table:
//   1. it claims the triage items before the roots, which sit in a lower tier;
//   2. each answer is a `triaged` record by the agent on the root: a plain tier,
//      a dependency, a split, and an unclear one that becomes a question;
//   3. the answers are proposals: the admitted tiers stand, nothing is linked,
//      and no child exists until a person acts;
//   4. a person adopts one tier with the tier route and links one dependency;
//   5. refused answers (an unknown dependency, a root the agent does not hold
//      the triage item of) leave nothing on the log.
//
//   cargo build -p kbl-mcp -p kbl-tool -p kbl-tracker && node dev/check-tracker-triage.mjs
import {
  agent as spawnAgent,
  check,
  createRun,
  felixToken,
  giveUpAfter,
  killAll,
  newRunId,
  personCall,
  personRecord,
  startTracker,
  TRACKER_URL,
  workLog,
} from "./lib/stack.mjs";

const RUN = newRunId("trackertriage");
// w1 may see items of the repo the roots belong to, so one root can depend on another.
const tokens = {
  agents: { "tok-w1": "w1" },
  people: { "tok-gab": "gab" },
  grants: { w1: ["check/repo"] },
};
const timing = { KBL_CLAIM_VISIBILITY_MS: "8000", KBL_EXTEND_EVERY_MS: "1000" };

giveUpAfter(240, "the triage check");

// What the scripted agent answers, by the word its root's title starts with.
const SCRIPT = {
  "tri-plain": { tier: 1, why: "a crash on a common path" },
  "tri-needs": { tier: 2, why: "waits on the plain fix", depends_on: ["tri-plain"] },
  "tri-big": { tier: 1, why: "two changes in one", split: ["fix the reader", "fix the writer"] },
  "tri-vague": { tier: 2, why: "no repro in the issue", unclear: "acceptance" },
};
const ADMITTED_TIER = 3;
const ids = Object.keys(SCRIPT);

const felix = await felixToken();
createRun(RUN, "triage check");
const agent = (name) =>
  spawnAgent(felix, RUN, name, {
    ...timing,
    KBL_TRACKER_URL: TRACKER_URL,
    KBL_TRACKER_TOKEN: `tok-${name}`,
  });

// A root as ingest admits it: accepted at the rule's tier, with a triage item.
async function admit(id) {
  await personRecord("tok-gab", { kind: "imported", item: id, source: "check", upstream_id: `check/repo#${ids.indexOf(id) + 1}`, title: `${id} issue` });
  await personRecord("tok-gab", { kind: "accepted", item: id, tier: ADMITTED_TIER });
  await personRecord("tok-gab", {
    kind: "proposed",
    item: `${id}-t`,
    parent: id,
    key: "triage",
    work: "triage",
    title: `Triage: ${id} issue`,
  });
}

async function refusal(call) {
  try {
    await call();
    return null;
  } catch (e) {
    return e.message;
  }
}

const recordsOf = (kind) => workLog().filter((r) => r.kind === kind);

await startTracker(tokens);
try {
  const w1 = await agent("w1");
  for (const id of Object.keys(SCRIPT)) await admit(id);

  // The scripted agent: take work until every triage item is answered.
  const answered = new Set();
  const until = Date.now() + 90_000;
  while (answered.size < Object.keys(SCRIPT).length) {
    if (Date.now() > until) throw new Error("the scripted agent did not finish the triage items");
    const claim = await w1.tool("work_next", { wait_seconds: 3 });
    if (!claim.claimed) continue;
    const context = await w1.tool("work_context", {});
    if (context.work !== "triage") {
      // Work an earlier check left on the shared log is not this agent's job.
      await w1.tool("work_release", { item: context.item });
      continue;
    }
    if (!(context.parent in SCRIPT)) {
      await w1.tool("work_release", { item: context.item });
      continue;
    }
    const root = context.parent;

    if (answered.size === 0) {
      // Refused answers leave nothing behind.
      const before = workLog().length;
      const unknown = await refusal(() =>
        w1.tool("work_triage", { tier: 1, why: "w", depends_on: ["no-such-item"] }),
      );
      check(unknown?.includes("depends_on"), "a dependency that is no item is refused");
      const other = Object.keys(SCRIPT).find((id) => id !== root);
      const foreign = await refusal(() => w1.tool("work_triage", { item: `${other}-t`, tier: 1, why: "w" }));
      check(foreign !== null, "an answer for a triage item the agent does not hold is refused");
      const oversplit = await refusal(() =>
        w1.tool("work_triage", { tier: 1, why: "w", split: Array.from({ length: 9 }, (_, n) => `part ${n}`) }),
      );
      check(oversplit?.includes("at most"), "a split over the cap is refused");
      check(workLog().length === before, "the refused answers wrote nothing");
    }

    const answer = SCRIPT[root];
    const reply = await w1.tool("work_triage", answer);
    check(reply.triaged === root, `the answer for ${root} landed on ${root}`);
    const again = await w1.tool("work_triage", answer);
    check(again.offset === reply.offset, `the same answer for ${root} again wrote nothing`);
    await w1.tool("work_complete", { item: context.item });
    answered.add(root);
  }

  const triaged = recordsOf("triaged").filter((r) => r.item in SCRIPT);
  check(triaged.length === 4 && triaged.every((r) => r.by === "w1" && r.actor === "agent"), "four answers, each the agent's");
  const of = (id) => triaged.find((r) => r.item === id);
  check(of("tri-plain").tier === 1 && of("tri-plain").depends_on === undefined, "a plain answer carries a tier and nothing else");
  check(of("tri-needs").depends_on?.join() === "plain", "the dependency is on the record");
  check(of("tri-big").split?.length === 2, "the split titles are on the record");
  check(of("tri-vague").unclear === "acceptance", "an unclear answer says what is unclear");

  // Answers are proposals: tiers stand, nothing is linked, no child appears.
  for (const id of Object.keys(SCRIPT)) {
    const accepted = recordsOf("accepted").filter((r) => r.item === id);
    check(accepted.length === 1 && accepted[0].tier === ADMITTED_TIER, `${id} keeps the tier it was admitted at`);
  }
  check(!recordsOf("linked").some((r) => r.item in SCRIPT), "no dependency was linked by the answer");
  check(!workLog().some((r) => r.kind === "proposed" && r.title === "fix the reader" && r.parent in SCRIPT), "the split created no item");

  // A person acts on two of them.
  await personCall("tok-gab", "/v1/work/tier", { item: "tri-plain", tier: of("tri-plain").tier });
  await personRecord("tok-gab", { kind: "linked", item: "tri-needs", to: "tri-plain" });
  check(recordsOf("tier_changed").some((r) => r.item === "tri-plain" && r.tier === 1), "a person adopted the proposed tier");
  check(recordsOf("linked").filter((r) => r.item in SCRIPT).length === 1, "a person linked the dependency");
  const asAgent = await fetch(`${TRACKER_URL}/v1/work/tier`, {
    method: "POST",
    headers: { "content-type": "application/json", authorization: "Bearer tok-w1" },
    body: JSON.stringify({ item: "tri-big", tier: 1 }),
  });
  check(asAgent.status === 403, "an agent cannot change a tier");
} finally {
  killAll();
}
