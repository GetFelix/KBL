// The triage acceptance policy against the dev stack. Roots are imported the way
// admit mode leaves them (not accepted, one triage item each), a scripted agent
// answers each triage item, and the tracker runs with a policy file:
//   1. a routine docs triage is accepted by the tracker, naming the rule, and an
//      agent is handed its fix;
//   2. a safety-tagged triage and a replication triage stay in Needs you, and so does
//      one the triage marked unclear;
//   3. a person takes back a policy acceptance: the item returns to Triage and the
//      policy does not accept it again;
//   4. a person accepts a held item themselves, and that stays a person's acceptance.
//
//   cargo build -p kbl-mcp -p kbl-tool -p kbl-tracker && node dev/check-tracker-policy.mjs
import { mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
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
  raiseFormat,
  sleep,
  startTracker,
  TRACKER_URL,
  workLog,
} from "./lib/stack.mjs";

const RUN = newRunId("trackerpolicy");
const tokens = {
  agents: { "tok-w1": "w1" },
  people: { "tok-gab": "gab" },
  grants: { w1: ["check:check/repo"] },
};
const timing = { KBL_CLAIM_VISIBILITY_MS: "8000", KBL_EXTEND_EVERY_MS: "1000" };

giveUpAfter(240, "the triage policy check");

// Routine work a person does not need to see; replication, storage and anything
// needing a safety review stay with a person.
const policy = {
  rules: [
    {
      name: "routine",
      tiers: [2, 3],
      deny_areas: ["replication", "storage"],
      deny_needs: ["safety-review"],
    },
  ],
};
const dir = mkdtempSync(join(tmpdir(), "policy-"));
const policyFile = join(dir, "triage-policy.json");
writeFileSync(policyFile, JSON.stringify(policy));

// What the scripted agent answers, by the word its root's title starts with.
const SCRIPT = {
  "pol-docs": { tier: 2, why: "a typo in the docs", area: "docs" },
  "pol-docs2": { tier: 3, why: "a broken link", area: "docs" },
  "pol-safe": { tier: 2, why: "touches the ack path", area: "docs", needs: ["safety-review"] },
  "pol-repl": { tier: 2, why: "a leader change", area: "replication" },
  "pol-vague": { tier: 2, why: "no repro in the issue", unclear: "acceptance" },
};
const ids = Object.keys(SCRIPT);

const felix = await felixToken();
createRun(RUN, "triage policy check");
const agent = (name) =>
  spawnAgent(felix, RUN, name, {
    ...timing,
    KBL_TRACKER_URL: TRACKER_URL,
    KBL_TRACKER_TOKEN: `tok-${name}`,
  });

const recordsOf = (kind) => workLog().filter((r) => r.kind === kind);
const acceptsOf = (id) => recordsOf("accepted").filter((r) => r.item === id);

await startTracker(tokens, { KBL_TRIAGE_POLICY_FILE: policyFile });
try {
  await raiseFormat("tok-gab", 25);
  await personRecord("tok-gab", {
    kind: "rules_set",
    target: { source: "check", repo: "check/repo" },
    vocabulary: { areas: ["docs", "replication", "storage"], needs: ["safety-review"] },
  });
  // Roots as admit mode leaves them: imported, not accepted, with a triage item.
  for (const [n, id] of ids.entries()) {
    await personRecord("tok-gab", { kind: "imported", item: id, source: "check", upstream_id: `check/repo#${n + 1}`, title: `${id} issue` });
    await personRecord("tok-gab", { kind: "proposed", item: `${id}-t`, parent: id, key: "triage", work: "triage", title: `Triage: ${id} issue` });
  }

  const w1 = await agent("w1");
  // A fresh admit import hands out only triage first: no root is next before its triage is accepted.
  const first = await w1.tool("work_peek", { limit: 50 });
  check(!first.next.some((i) => ids.includes(i.item)), "no root is handed out before its triage is accepted");
  check(ids.every((id) => first.next.some((i) => i.item === `${id}-t`)), "every root's triage item is next in line");
  const answered = new Set();
  const until = Date.now() + 90_000;
  while (answered.size < ids.length) {
    if (Date.now() > until) throw new Error("the scripted agent did not finish the triage items");
    const claim = await w1.tool("work_next", { wait_seconds: 3, kinds: ["triage"] });
    if (!claim.claimed) continue;
    const context = await w1.tool("work_context", {});
    if (context.work !== "triage" || !(context.parent in SCRIPT)) {
      await w1.tool("work_release", { item: context.item });
      continue;
    }
    await w1.tool("work_triage", SCRIPT[context.parent]);
    await w1.tool("work_complete", { item: context.item });
    answered.add(context.parent);
  }
  await sleep(1500);

  // 1. The routine triages were accepted by the tracker, naming the rule.
  for (const id of ["pol-docs", "pol-docs2"]) {
    const [accepted, ...more] = acceptsOf(id);
    check(accepted && more.length === 0, `${id} was accepted once`);
    check(accepted.actor === "tracker" && accepted.admitted_by === "routine", `${id}'s acceptance is the tracker's and names the rule`);
  }
  // 2. Everything else waits for a person.
  for (const id of ["pol-safe", "pol-repl", "pol-vague"]) {
    check(acceptsOf(id).length === 0, `${id} was left for a person`);
  }

  // 3. A person takes back one policy acceptance, before any agent has the item.
  await personCall("tok-gab", "/v1/work/proposals/undo-admission", { item: "pol-docs2" });
  check(recordsOf("admission_undone").some((r) => r.item === "pol-docs2" && r.actor === "person"), "the take-back is a person's record");
  await sleep(4000);
  check(acceptsOf("pol-docs2").length === 1, "the policy did not accept pol-docs2 again");
  const nonPerson = await fetch(`${TRACKER_URL}/v1/work/proposals/undo-admission`, {
    method: "POST",
    headers: { "content-type": "application/json", authorization: "Bearer tok-w1" },
    body: JSON.stringify({ item: "pol-docs" }),
  });
  check(nonPerson.status === 403, "an agent cannot take back a policy acceptance");

  // 4. A person accepts a held item; that acceptance is theirs.
  await personCall("tok-gab", "/v1/work/proposals/accept", { item: "pol-safe" });
  const mine = acceptsOf("pol-safe");
  check(mine.length === 1 && mine[0].actor === "person" && mine[0].admitted_by === undefined, "a person's acceptance names no rule");

  // Only accepted items are next in line: the policy's and the person's, nothing else.
  const peek = await w1.tool("work_peek", { kinds: ["fix"], limit: 50 });
  const next = new Set(peek.next.map((i) => i.item));
  check(next.has("pol-docs") && next.has("pol-safe"), "the policy's item and the person's item are next in line");
  for (const id of ["pol-docs2", "pol-repl", "pol-vague"]) check(!next.has(id), `${id} is not next in line`);
} finally {
  killAll();
}
