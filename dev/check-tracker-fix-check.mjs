// Drives a failed check on an agent's pull request through the dev stack, with
// a stand-in for GitHub and the real ingest, tracker and agent tools:
//   1. an agent fixes an imported issue and reports its pull request;
//   2. a poll finds the check red; ingest reads the job's log and the tracker
//      records check_failed and opens one fix_check item;
//   3. an agent claims it with the head commit and the log excerpt in its claim;
//   4. the agent finishes, GitHub shows the check green on a new head, and a
//      later poll records the green status and opens nothing more.
// The stand-in answers the job log directly. The redirect to a signed link that
// GitHub sends is not driven here: ingest follows it only to https.
//
//   cargo build -p kbl-tool -p kbl-mcp -p kbl-tracker -p kbl-ingest && node dev/check-tracker-fix-check.mjs
import { spawn } from "node:child_process";
import { mkdtempSync, readFileSync, writeFileSync } from "node:fs";
import { createServer } from "node:http";
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
  startTracker,
  TRACKER_URL,
  workLog,
} from "./lib/stack.mjs";

const INGEST = process.env.KBL_INGEST || "target/debug/kbl-ingest";
const REPO = `check/fixcheck-${Date.now().toString(36)}`;
const PR_NUMBER = 9001;
const PR = `${REPO}#${PR_NUMBER}`;
const LOG = "error[E0432]: unused import `Foo` in src/lib.rs";
const RUN = newRunId("fixcheck");
const tokens = {
  agents: { "tok-w1": "w1", "tok-w2": "w2" },
  people: { "tok-gab": "gab" },
  ingest: { "tok-ingest": "ingest" },
};

giveUpAfter(180, "the fix_check check");

const issue = JSON.parse(readFileSync(new URL("../ingest/fixtures/issue.json", import.meta.url), "utf8"));
issue.url = `https://api.github.com/repos/${REPO}/issues/812`;
issue.repository_url = `https://api.github.com/repos/${REPO}`;

let root;
// What GitHub shows of the pull request's head commit.
let head = { sha: "sha-red", conclusion: "failure" };
const run = () => ({
  name: "lint",
  status: "completed",
  conclusion: head.conclusion,
  html_url: `https://github.com/${REPO}/actions/runs/5/job/55`,
  output: { title: "lint", summary: "lint failed", text: null },
});

const github = createServer((req, res) => {
  const path = new URL(req.url, "http://gh").pathname;
  const answer = (status, body) => {
    res.writeHead(status, { "content-type": "application/json" });
    res.end(JSON.stringify(body));
  };
  if (req.method !== "GET") return answer(404, { message: "Not Found" });
  if (path === `/repos/${REPO}/issues`) return answer(200, [issue]);
  if (path === `/repos/${REPO}/issues/812`) return answer(200, issue);
  if (path === `/repos/${REPO}/pulls/${PR_NUMBER}`) {
    return answer(200, {
      number: PR_NUMBER,
      state: "open",
      merged: false,
      draft: false,
      body: "",
      user: { login: "kbl[bot]" },
      // Created long after the claim, on the branch the tracker derives from the item.
      created_at: "2099-01-01T00:00:00Z",
      head: { sha: head.sha, ref: `kbl/${root}` },
      base: { ref: "main", repo: { default_branch: "main" } },
      requested_reviewers: [],
      requested_teams: [],
    });
  }
  if (path === `/repos/${REPO}/pulls/${PR_NUMBER}/reviews`) return answer(200, []);
  if (path === `/repos/${REPO}/commits/${head.sha}/check-runs`) {
    return answer(200, { total_count: 1, check_runs: [run()] });
  }
  if (path === `/repos/${REPO}/commits/${head.sha}/status`) return answer(200, { total_count: 0, statuses: [] });
  if (path === `/repos/${REPO}/actions/jobs/55/logs`) {
    res.writeHead(200, { "content-type": "text/plain" });
    return res.end(`${"setup\n".repeat(50)}${LOG}\n`);
  }
  if (
    path === `/repos/${REPO}/issues/comments` ||
    path === `/repos/${REPO}/issues/812/comments` ||
    path === `/repos/${REPO}/issues/812/dependencies/blocked_by` ||
    path === `/repos/${REPO}/issues/812/sub_issues`
  ) {
    return answer(200, []);
  }
  answer(404, { message: "Not Found" });
});
await new Promise((resolve) => github.listen(0, "127.0.0.1", resolve));

const dir = mkdtempSync(join(tmpdir(), "fixcheck-"));
const configFile = join(dir, "ingest.json");
writeFileSync(
  configFile,
  JSON.stringify({
    tracker: { url: TRACKER_URL, token_env: "KBL_CHECK_INGEST_TOKEN" },
    github: {
      api: `http://127.0.0.1:${github.address().port}`,
      token_env: "KBL_CHECK_GITHUB_TOKEN",
      own_logins: ["kbl[bot]"],
    },
    rules: [{ name: "agent-ok", repo: REPO, mode: "admit", labels_all: ["agent-ok"], triage: "skip" }],
  }),
);

async function pollOnce() {
  const child = spawn(INGEST, ["--config", configFile, "--once"], {
    env: { ...process.env, KBL_CHECK_INGEST_TOKEN: "tok-ingest", KBL_CHECK_GITHUB_TOKEN: "unused" },
    stdio: ["ignore", "inherit", "inherit"],
  });
  const code = await new Promise((resolve) => child.on("exit", resolve));
  if (code !== 0) throw new Error(`kbl-ingest --once exited with ${code}`);
}

async function takeNext(who, seconds) {
  const until = Date.now() + seconds * 1000;
  for (;;) {
    const reply = await who.tool("work_next", { wait_seconds: 3 });
    if (reply.claimed || Date.now() > until) return reply;
  }
}

const mine = (kind) => workLog().filter((r) => r.kind === kind && r.item === root);

const felix = await felixToken();
createRun(RUN, "fix_check check");
const agent = (name) =>
  spawnAgent(felix, RUN, name, {
    KBL_CLAIM_VISIBILITY_MS: "8000",
    KBL_EXTEND_EVERY_MS: "1000",
    KBL_TRACKER_URL: TRACKER_URL,
    KBL_TRACKER_TOKEN: `tok-${name}`,
  });

await startTracker(tokens);
try {
  const [w1, w2] = await Promise.all([agent("w1"), agent("w2")]);
  await personRecord("tok-gab", { kind: "format_raised", to: 23 });

  // 1. The issue becomes a root, and an agent fixes it and names its pull request.
  await pollOnce();
  const imported = workLog().find((r) => r.kind === "imported" && r.upstream_id === `${REPO}#812`);
  check(imported !== undefined, "the issue was imported");
  root = imported.item;
  const first = await takeNext(w1, 20);
  check(first.claimed && first.item.id === root, "w1 claimed the root");
  await w1.tool("decide", {
    phase: "verify",
    so_far: "fix in",
    why: "the test fails without the fix",
    commit: "abc123",
    revert_check: { test: "t", failed_without_fix: true, passed_with_fix: true },
  });
  await w1.tool("work_progress", { note: "fixed", pr: PR });
  await w1.tool("work_complete", { item: root });

  // 2. The check is red on the pull request's head.
  await pollOnce();
  const failed = mine("check_failed");
  check(failed.length === 1, "one check_failed was recorded for the root");
  check(failed[0].check === "lint" && failed[0].head === "sha-red", "it names the check and the head commit");
  check(failed[0].log.includes(LOG), "it carries the end of the job's log");
  check(failed[0].fix === true, "the tracker opened a fix for it");
  await pollOnce();
  check(mine("check_failed").length === 1, "a poll of the same failure recorded nothing more");

  // 3. An agent claims the fix with the evidence.
  const fix = await takeNext(w2, 20);
  check(fix.claimed && fix.item.work === "fix_check" && fix.item.parent === root, "w2 claimed a fix_check child of the root");
  check(fix.item.fix_check?.head === "sha-red" && fix.item.pr === PR, "the claim names the head commit and the pull request");
  check(JSON.stringify(fix).includes("unused import"), "the claim carries the log excerpt");

  // 4. Its push turns the check green.
  await w2.tool("work_complete", { item: fix.item.id });
  head = { sha: "sha-green", conclusion: "success" };
  await pollOnce();
  const statuses = mine("pr_status");
  check(statuses.at(-1)?.status.checks === "green", "the root's last pull request status is green");
  check(mine("check_failed").length === 1, "the green head recorded no second failure");
} finally {
  if (root) await personCall("tok-gab", "/v1/work/close", { item: root }).catch(() => {});
  killAll();
  github.close();
}
