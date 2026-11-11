// Runs review as work against the dev stack. An author's pull request opens,
// the tracker makes a first-pass and a final review, and reviewers claim them
// by tier:
//   1. the author never gets a review of its own request, and a reviewer gets
//      none unless it names the tier;
//   2. findings become a fix for the author's tree, a proposal for a person,
//      or nothing;
//   3. the final reviewer cannot recommend a merge while a fix is open, and
//      its verdict is a record while the root stays open for a person.
//
//   cargo build -p felix-mcp -p agents-tool -p agents-tracker && node dev/check-tracker-review.mjs
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

const RUN = newRunId("trackerreview");
const PR = "GetFelix/felix#7701";
const tokens = {
  agents: { "tok-w1": "w1", "tok-w2": "w2", "tok-w3": "w3", "tok-w4": "w4" },
  people: { "tok-gab": "gab" },
  ingest: { "tok-ing": "ingest" },
};
const timing = { AGENTS_CLAIM_VISIBILITY_MS: "8000", AGENTS_EXTEND_EVERY_MS: "1000" };

giveUpAfter(240, "the tracker review check");

const felix = await felixToken();
createRun(RUN, "review check");
const agent = (name) =>
  spawnAgent(felix, RUN, name, {
    ...timing,
    AGENTS_TRACKER_URL: TRACKER_URL,
    AGENTS_TRACKER_TOKEN: `tok-${name}`,
  });

async function takeNext(who, seconds, args = {}) {
  const until = Date.now() + seconds * 1000;
  for (;;) {
    const reply = await who.tool("work_next", { wait_seconds: 3, ...args });
    if (reply.claimed || Date.now() > until) return reply;
  }
}

async function refusal(call) {
  try {
    await call();
    return null;
  } catch (e) {
    return e.message;
  }
}

async function ingest(record) {
  const response = await fetch(`${TRACKER_URL}/v1/ingest/records`, {
    method: "POST",
    headers: { "content-type": "application/json", authorization: "Bearer tok-ing" },
    body: JSON.stringify({ record }),
  });
  const body = await response.json();
  if (!response.ok) throw new Error(`ingest ${record.kind}: ${body.why ?? body.error}`);
  return body;
}

const status = (review) => ({
  kind: "pr_status",
  item: "rvroot",
  status: { pr: PR, phase: "open", review, checks: "green" },
});

const tracker = await startTracker(tokens);
try {
  const [w1, w2, w3, w4] = await Promise.all([agent("w1"), agent("w2"), agent("w3"), agent("w4")]);

  // The author fixes rvroot and opens its request.
  await personRecord("tok-gab", { kind: "imported", item: "rvroot", source: "github", upstream_id: PR, title: "stale read" });
  await personRecord("tok-gab", { kind: "accepted", item: "rvroot", tier: 1 });
  const claim = await takeNext(w1, 20);
  check(claim.claimed && claim.item.id === "rvroot", "w1 claimed rvroot");
  await w1.tool("decide", {
    phase: "verify",
    so_far: "fix in, test added",
    why: "the test fails without the fix",
    commit: "abc123",
    revert_check: { test: "reads_see_a_fresh_write", failed_without_fix: true, passed_with_fix: true },
  });
  await w1.tool("work_progress", { note: "fixed", pr: PR });
  await w1.tool("work_complete", { item: "rvroot" });
  const checked = workLog().find((r) => r.kind === "decision" && r.item === "rvroot");
  check(checked?.revert_check?.test === "reads_see_a_fresh_write", "the decision carries its revert check");

  // 1. Opening the request makes the reviews, once.
  await ingest(status("unreviewed"));
  await ingest(status("approved"));
  const reviews = workLog().filter((r) => r.kind === "proposed" && r.work === "review");
  check(reviews.length === 2, "two reviews were made for the request, and a second status made none");
  check(reviews.map((r) => r.review.tier).join() === "fast,deep", "the first pass is for the fast tier and the final for the deep tier");

  const plain = await w2.tool("work_next", { wait_seconds: 1 });
  check(!plain.claimed, "an agent that names no tier is given no review");
  const own = await w1.tool("work_next", { wait_seconds: 1, review_tiers: ["fast"] });
  check(!own.claimed, "the author is given no review of its own request");
  const wrongTier = await w2.tool("work_next", { wait_seconds: 1, review_tiers: ["deep"] });
  check(!wrongTier.claimed, "the final review waits for the first pass");
  const unknown = await refusal(() => w2.tool("work_next", { wait_seconds: 1, review_tiers: ["slow"] }));
  check(unknown?.includes("reviewer tier"), "a tier that is not configured is refused");

  const first = await takeNext(w2, 20, { review_tiers: ["fast"] });
  console.log("first-pass reply:", JSON.stringify(first));
  check(first.claimed && first.item.work === "review" && first.item.review.pass === "first", "w2 was handed the first-pass review");
  check(first.item.pr === PR && first.item.pr_branch === "kibble/rvroot", "the review names the request and the author's branch");

  // 2. Findings.
  const f = (verdict, title) => ({
    severity: "high",
    file: "src/read.rs",
    line: 88,
    title,
    scenario: "restart between fill and read returns the old value",
    verdict,
  });
  const noFile = await refusal(() => w2.tool("work_finding", { ...f("fix", "x"), file: "" }));
  check(noFile !== null, "a finding with no file is refused");
  const bad = await refusal(() => w2.tool("work_finding", { ...f("ship", "x") }));
  check(bad?.includes("verdict"), "a finding verdict is fix, defer or dismiss");
  await w2.tool("work_finding", f("fix", "stale read after restart"));
  await w2.tool("work_finding", f("defer", "cache has no size bound"));
  await w2.tool("work_finding", f("dismiss", "style nit"));
  const findings = workLog().filter((r) => r.kind === "finding");
  check(findings.length === 3 && findings.every((r) => r.by === "w2"), "three findings were recorded from w2");
  await w2.tool("work_complete", { item: first.item.id });

  // The fix is work in the author's tree, and the deferred one waits for a person.
  const fix = await takeNext(w3, 20);
  check(fix.claimed && fix.item.work === "fix" && fix.item.parent === "rvroot", "w3 was handed the fix as a child of rvroot");
  check(fix.item.pr_branch === "kibble/rvroot", "the fix goes on the request's branch");
  const none = await w4.tool("work_next", { wait_seconds: 1 });
  check(!none.claimed, "the deferred finding is not claimable until a person accepts it");

  // 3. The final review.
  const last = await takeNext(w4, 20, { review_tiers: ["deep"] });
  check(last.claimed && last.item.review.pass === "final", "w4 was handed the final review");
  const early = await refusal(() => w4.tool("work_verdict", { verdict: "merge", why: "looks good" }));
  check(early?.includes("open_children"), "a merge cannot be recommended while a fix is open");
  await w3.tool("work_complete", { item: fix.item.id });
  const noVerdict = await refusal(() => w4.tool("work_complete", { item: last.item.id }));
  check(noVerdict?.includes("missing_field"), "the final review cannot finish without a verdict");
  await w4.tool("work_verdict", { verdict: "merge", why: "the fix landed and the test guards it" });
  await w4.tool("work_complete", { item: last.item.id });
  const verdict = workLog().find((r) => r.kind === "review_verdict");
  check(verdict?.verdict === "merge" && verdict.by === "w4", "the verdict is a record from the final reviewer");
  check(!workLog().some((r) => r.kind === "closed" && r.item === "rvroot"), "nothing merged: a person does that");

  // Agents cannot write a finding as a person.
  const asAgent = await refusal(() => personRecord("tok-w2", { kind: "finding", item: first.item.id }));
  check(asAgent !== null, "an agent token cannot write findings as a person");
  void tracker;
} finally {
  killAll();
}
