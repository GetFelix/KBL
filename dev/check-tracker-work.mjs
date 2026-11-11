// Runs the work tools against the dev stack, each agent a felix-mcp process
// that calls the tracker over REST, each person a REST call:
//   1. w1 claims an item, reads its context, posts two children and orders
//      them, records a decision, a step and an edit, and posts a proposal;
//      w2 finishes the children in order; w1 completes the item;
//   2. a person accepts the proposal at a new tier and an agent finishes it;
//   3. an agent asks a question and a person answers it;
//   4. refused calls (an item the agent does not hold, a decision without its
//      commit) leave nothing on the log.
//
//   cargo build -p felix-mcp -p agents-tool -p agents-tracker && node dev/check-tracker-work.mjs
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

const RUN = newRunId("trackerwork");
const tokens = {
  agents: { "tok-w1": "w1", "tok-w2": "w2", "tok-w3": "w3" },
  people: { "tok-gab": "gab" },
};
const timing = { AGENTS_CLAIM_VISIBILITY_MS: "8000", AGENTS_EXTEND_EVERY_MS: "1000" };

giveUpAfter(240, "the tracker work check");

const felix = await felixToken();
// The run owns the artifacts cache that long outputs are stored in.
createRun(RUN, "work tools check");
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

async function takeNext(who, seconds) {
  const until = Date.now() + seconds * 1000;
  for (;;) {
    const reply = await who.tool("work_next", { wait_seconds: 3 });
    if (reply.claimed || Date.now() > until) return reply;
  }
}

// What a call throws, or null if it succeeded.
async function refusal(call) {
  try {
    await call();
    return null;
  } catch (e) {
    return e.message;
  }
}

const recordsOf = (item) => workLog().filter((r) => r.item === item);

const tracker = await startTracker(tokens);
try {
  const [w1, w2, w3] = await Promise.all([agent("w1"), agent("w2"), agent("w3")]);

  // 1. One agent splits its item; another finishes the pieces.
  await seed("root1", 1);
  const first = await takeNext(w1, 20);
  check(first.claimed && first.item.id === "root1", "w1 claimed root1");

  const context = await w1.tool("work_context", {});
  check(context.item === "root1" && context.context.length === 0, "root1 starts with no context");
  await w1.tool("work_context", { entry: "repro", text: "run the suite twice" });
  const read = await w1.tool("work_context", {});
  check(read.context[0]?.entry === "repro" && read.context[0].text.text === "run the suite twice", "the context entry reads back");

  const c1 = await w1.tool("work_post", { key: "reader", work: "split", title: "fix the reader" });
  const c2 = await w1.tool("work_post", { key: "tests", work: "test", title: "cover the reader" });
  check(c1.created && c2.created && c1.item !== c2.item, "w1 posted two children");
  const retry = await w1.tool("work_post", { key: "reader", work: "split", title: "fix the reader" });
  check(retry.item === c1.item && retry.created === false, "the same key again returned the first child");
  await w1.tool("work_link", { item: c2.item, waits_on: c1.item });
  const cycle = await refusal(() => w1.tool("work_link", { item: c1.item, waits_on: c2.item }));
  check(cycle?.includes("cycle"), "a link that closes a cycle is refused");
  const early = await refusal(() => w1.tool("work_complete", { item: "root1" }));
  check(early?.includes("open_children"), "root1 cannot complete with open children");

  const decision = await w1.tool("decide", {
    phase: "cause",
    so_far: "the reader runs before the writer fills the cache",
    question: "which side is wrong",
    options: ["reader", "writer"],
    chosen: "reader",
    why: "the writer holds the lock",
    commit: "abc123",
    evidence: [{ stream: "work.events", offset: 0 }],
  });
  await w1.tool("record_step", { command: "cargo test", exit_code: 101, output: "1 failed" });
  await w1.tool("record_step", { command: "cargo test", exit_code: 0, output: "x".repeat(6000) });
  await w1.tool("edit", { file: "src/cache.rs", hunk: "@@ -1 +1 @@", decision: decision.offset });
  await w1.tool("work_progress", { note: "reader fixed", branch: "fix/reader" });
  const proposal = await w1.tool("work_post", { key: "found", work: "proposal", title: "the writer leaks a handle" });

  const rootKinds = recordsOf("root1").map((r) => r.kind);
  for (const kind of ["context", "decision", "step", "edit", "progress"]) {
    check(rootKinds.includes(kind), `root1 has a ${kind} record`);
  }
  const steps = recordsOf("root1").filter((r) => r.kind === "step");
  check(steps[1].output.artifact?.startsWith("b-"), "a long step output was stored as an artifact");
  check(
    recordsOf("root1").filter((r) => ["decision", "step", "edit"].includes(r.kind)).every((r) => r.by === "w1" && r.attempt === 1),
    "the records name w1 and attempt 1",
  );

  // 4, early: refusals leave nothing on the log.
  const before = workLog().length;
  const stranger = await refusal(() => w2.tool("work_post", { item: "root1", key: "x", work: "split", title: "not mine" }));
  check(stranger?.includes("hold"), "w2 cannot post under an item it does not hold");
  const noCommit = await refusal(() => w1.tool("decide", { phase: "fix", so_far: "s", why: "w", commit: "" }));
  check(noCommit?.includes("missing_field"), "a decision without a commit is refused");
  const noEdit = await refusal(() => w1.tool("edit", { file: "f", hunk: "h", decision: 99999 }));
  check(noEdit?.includes("unknown_decision"), "an edit that cites no decision is refused");
  check(workLog().length === before, "the refused calls wrote nothing");

  // w2 takes the children in order: the second waits on the first.
  const t1 = await takeNext(w2, 20);
  check(t1.claimed && t1.item.id === c1.item, "w2 was handed the first child");
  const blocked = await w2.tool("work_next", { wait_seconds: 1 }).catch((e) => ({ claimed: false, why: e.message }));
  check(!blocked.claimed, "the second child waits for the first");
  await w2.tool("work_complete", { item: c1.item });
  const t2 = await takeNext(w2, 20);
  check(t2.claimed && t2.item.id === c2.item, "w2 was handed the second child after the first finished");
  await w2.tool("work_complete", { item: c2.item });
  await w1.tool("work_complete", { item: "root1", cost_micros: 5000 });
  check(recordsOf("root1").some((r) => r.kind === "done" && r.by === "w1"), "w1 completed root1");

  // 2. A person accepts the proposal.
  const proposed = recordsOf(proposal.item).find((r) => r.kind === "proposed");
  check(proposed && proposed.by === "w1" && proposed.parent === undefined, "the proposal is on the log, from w1, with no parent");
  const none = await w2.tool("work_next", { wait_seconds: 1 });
  check(!none.claimed, "a proposal is not claimable before a person accepts it");
  await personCall("tok-gab", "/v1/work/proposals/edit", { item: proposal.item, title: "the writer leaks a file handle", tier: 3 });
  await personCall("tok-gab", "/v1/work/proposals/accept", { item: proposal.item });
  await personCall("tok-gab", "/v1/work/tier", { item: proposal.item, tier: 0 });
  const found = await takeNext(w2, 20);
  check(found.claimed && found.item.id === proposal.item && found.item.tier === 0, "w2 was handed the accepted proposal at the new tier");
  await w2.tool("work_complete", { item: proposal.item });
  const wrong = await refusal(() => personCall("tok-gab", "/v1/work/proposals/reject", { item: proposal.item }));
  check(wrong?.includes("not_a_proposal"), "an accepted item cannot be rejected");

  // 3. A question and its answer.
  await seed("root2", 1);
  const second = await takeNext(w3, 20);
  check(second.claimed && second.item.id === "root2", "w3 claimed root2");
  const asked = await w3.tool("work_ask", { question: "which cache is meant?" });
  await personCall("tok-gab", "/v1/work/questions/answer", { item: "root2", text: { text: "the read cache" } });
  const answer = recordsOf("root2").find((r) => r.kind === "answer");
  check(answer?.question === asked.offset && answer.actor === "person" && answer.by === "gab", "the person's answer names the question and who answered");
  const again = await refusal(() => personCall("tok-gab", "/v1/work/questions/answer", { item: "root2", text: { text: "again" } }));
  check(again?.includes("no_open_question"), "a question is answered once");
  await w3.tool("work_complete", { item: "root2" });

  // Agents cannot use the dashboard routes, and people cannot use the tools.
  const asAgent = await fetch(`${TRACKER_URL}/v1/work/close`, {
    method: "POST",
    headers: { "content-type": "application/json", authorization: "Bearer tok-w1" },
    body: JSON.stringify({ item: "root1" }),
  });
  check(asAgent.status === 403, "an agent token is refused on a dashboard route");
  void tracker;
} finally {
  killAll();
}
