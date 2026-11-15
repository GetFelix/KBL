// Runs kbl-ingest against the dev stack, with a stand-in for GitHub that lists
// one agent-ok issue every time it is asked:
//   1. the first poll imports the issue as a root, accepts it and posts its
//      triage and augment items;
//   2. two more polls of the same listing write nothing;
//   3. a poll after ingest's saved state is lost finds the root and writes nothing.
// Claiming each item once is dev/check-tracker-claims.mjs.
//
//   cargo build -p kbl-tool -p kbl-tracker -p kbl-ingest && node dev/check-tracker-ingest.mjs
import { spawnSync } from "node:child_process";
import { mkdtempSync, readFileSync, writeFileSync } from "node:fs";
import { createServer } from "node:http";
import { tmpdir } from "node:os";
import { join } from "node:path";
import {
  check,
  giveUpAfter,
  killAll,
  startTracker,
  TRACKER_URL,
  workLog,
} from "./lib/stack.mjs";

const INGEST = process.env.KBL_INGEST || "target/debug/kbl-ingest";
// A repo of its own, so records from an earlier run on the same stack do not count.
const REPO = `check/ingest-${Date.now().toString(36)}`;
const UPSTREAM = `${REPO}#812`;
const tokens = { ingest: { "tok-ingest": "ingest" } };

giveUpAfter(120, "the ingest check");

const issue = JSON.parse(readFileSync(new URL("../ingest/fixtures/issue.json", import.meta.url), "utf8"));
issue.url = `https://api.github.com/repos/${REPO}/issues/812`;
issue.repository_url = `https://api.github.com/repos/${REPO}`;

// GitHub as ingest reads it: the issue in every listing, no comments or links,
// and 404 for anything else (no releases, no pull requests).
const github = createServer((req, res) => {
  const path = new URL(req.url, "http://gh").pathname;
  const answer = (status, body) => {
    res.writeHead(status, { "content-type": "application/json" });
    res.end(JSON.stringify(body));
  };
  if (req.method !== "GET") return answer(404, { message: "Not Found" });
  if (path === `/repos/${REPO}/issues`) return answer(200, [issue]);
  if (path === `/repos/${REPO}/issues/812`) return answer(200, issue);
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

const dir = mkdtempSync(join(tmpdir(), "ingest-"));
const configFile = join(dir, "ingest.json");
writeFileSync(
  configFile,
  JSON.stringify({
    tracker: { url: TRACKER_URL, token_env: "KBL_CHECK_INGEST_TOKEN" },
    github: { api: `http://127.0.0.1:${github.address().port}`, token_env: "KBL_CHECK_GITHUB_TOKEN" },
    rules: [
      { name: "agent-ok", repo: REPO, mode: "admit", labels_all: ["agent-ok"], priority_labels: { p1: 1 } },
    ],
  }),
);

function pollOnce() {
  const run = spawnSync(INGEST, ["--config", configFile, "--once"], {
    env: { ...process.env, KBL_CHECK_INGEST_TOKEN: "tok-ingest", KBL_CHECK_GITHUB_TOKEN: "unused" },
    stdio: ["ignore", "inherit", "inherit"],
  });
  if (run.status !== 0) throw new Error(`kbl-ingest --once exited with ${run.status}`);
}

async function ingestCall(method, path, body) {
  const response = await fetch(`${TRACKER_URL}${path}`, {
    method,
    headers: { "content-type": "application/json", authorization: "Bearer tok-ingest" },
    body: body && JSON.stringify(body),
  });
  const reply = await response.json();
  if (!response.ok) throw new Error(`${method} ${path} -> ${response.status}: ${JSON.stringify(reply)}`);
  return reply;
}

let root;
// Every record of the root's tree, as kind:item.
const tree = () =>
  workLog()
    .filter((r) => r.item === root || r.parent === root || r.item?.startsWith(`${root}-`))
    .map((r) => `${r.kind}:${r.item}`);

await startTracker(tokens);
try {
  // 1. The first poll.
  pollOnce();
  const imported = workLog().filter((r) => r.kind === "imported" && r.upstream_id === UPSTREAM);
  check(imported.length === 1, `${UPSTREAM} was imported once`);
  root = imported[0].item;
  const first = tree();
  check(first.filter((k) => k === `accepted:${root}`).length === 1, "the root was accepted once");
  check(
    JSON.stringify(first.filter((k) => k.startsWith("proposed:")).sort()) ===
      JSON.stringify([`proposed:${root}-a`, `proposed:${root}-t`]),
    "the root has one triage item and one augment item",
  );

  // 2. The same listing twice more.
  pollOnce();
  pollOnce();
  check(JSON.stringify(tree()) === JSON.stringify(first), "two more polls of the same issue wrote nothing");

  // 3. Ingest loses its saved state, as after a restart with a lost save.
  const saved = await ingestCall("GET", "/v1/ingest/state/github");
  await ingestCall("PUT", "/v1/ingest/state/github", { state: null, version: saved.version });
  pollOnce();
  check(JSON.stringify(tree()) === JSON.stringify(first), "a poll with no saved state found the root and wrote nothing");
} finally {
  killAll();
  github.close();
}
