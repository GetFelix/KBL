// A backup, and a restore into a fresh Felix, hold the same state. docs/backup-restore.md.
//   1. Fill work.events the way a team does: people accept work, agents claim,
//      post, record decisions and steps, report usage and findings, a person
//      sets prices and answers a question, a pull request is reviewed.
//   2. Stop the tracker, export the log and digest its fold.
//   3. Throw the stack away (dev/up.sh), start a tracker on the empty one to
//      create the streams, stop it, import the backup, digest again. The two
//      digests must be equal, and so must the canonical state itself.
//   4. A tracker started on the restored log serves it: the item that was
//      waiting is handed out and finished, the finished one is not.
//   5. A damaged backup is refused, and an import into a log that holds other records is too.
//   6. Team mode: the organization's log, a second team's log and an agent token
//      minted in that team are backed up and restored the same way, and the
//      restored tracker serves the team to the token it had before.
//
// This ends by recreating the stack, so it runs after the other checks.
//   cargo build -p kbl-mcp -p kbl-tool -p kbl-tracker && node dev/check-backup.mjs
import { spawnSync } from "node:child_process";
import { mkdtempSync, readFileSync, statSync, writeFileSync } from "node:fs";
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
  personCall as personCallAny,
  sleep,
  startTracker,
  TRACKER_URL,
  workLog,
} from "./lib/stack.mjs";

const TOOL = process.env.KBL_TOOL || "target/debug/kbl-tool";
// Other checks share the log, and a request belongs to one item.
const PR = `GetFelix/felix#${100000 + (Date.now() % 800000)}`;
const tokens = {
  agents: { "tok-w1": "w1", "tok-w2": "w2", "tok-w3": "w3" },
  people: { "tok-gab": "gab" },
  ingest: { "tok-ing": "ingest" },
};
const timing = { KBL_CLAIM_VISIBILITY_MS: "8000", KBL_EXTEND_EVERY_MS: "1000" };
const dir = mkdtempSync(join(tmpdir(), "backup-"));
const file = join(dir, "work.backup");

giveUpAfter(600, "the backup check");

function tool(args, env = {}) {
  const out = spawnSync(TOOL, args, { encoding: "utf8", env: { ...process.env, ...env } });
  return { ok: out.status === 0, out: out.stdout.trim(), err: out.stderr.trim() };
}
function must(args, env) {
  const r = tool(args, env);
  if (!r.ok) throw new Error(`kbl-tool ${args.join(" ")}: ${r.err}`);
  return r.out;
}
const digest = (stateFile, backup) =>
  JSON.parse(must(["work", "digest", "--state", stateFile, ...(backup ? ["--backup", backup] : [])]));
const orgDigest = (stateFile, backup) =>
  JSON.parse(must(["org", "digest", "--state", stateFile, ...(backup ? ["--backup", backup] : [])]));
// Teams are on, so a person's token is in more than one team and every call names its team.
const DEFAULT = "default";
const personRecord = (token, record) =>
  personCallAny(token, `/v1/teams/${DEFAULT}/work/records`, { record });
const personCall = (token, path, body) =>
  personCallAny(token, path.replace(/^\/v1\//, `/v1/teams/${DEFAULT}/`), body);
const admin = { KBL_TRACKER_URL: TRACKER_URL, KBL_TRACKER_TOKEN: "tok-gab" };
const orgEnv = { KBL_ORG_NAMESPACE: "kbl" };
const ALPHA = { KBL_NAMESPACE: "alpha" };
const alphaApi = async (token, path, body) => {
  const response = await fetch(`${TRACKER_URL}/v1/teams/alpha/work/${path}`, {
    method: "POST",
    headers: { "content-type": "application/json", authorization: `Bearer ${token}` },
    body: JSON.stringify(body),
  });
  return { status: response.status, body: await response.json().catch(() => ({})) };
};
// A team create waits for the org writer lease, which a new tracker may not hold yet.
async function teamCreate(name) {
  for (let tries = 0; ; tries++) {
    const r = tool(["team", "create", name], admin);
    if (r.ok) return r.out;
    if (tries === 20) throw new Error(`team create ${name}: ${r.err}`);
    await sleep(1000);
  }
}

// Claims until `who` holds the item `match` names, clearing away what an earlier check left ready.
async function take(who, match, args = {}) {
  for (let tries = 0; tries < 20; tries++) {
    const reply = await who.tool("work_next", { wait_seconds: 3, ...args });
    if (!reply.claimed) continue;
    if (match(reply.item)) return reply;
    // Finished, not handed back, or it would be the next thing this agent is given.
    await who.tool("work_complete", { item: reply.item.id }).catch(() => who.tool("work_release", { item: reply.item.id, reason: "not this check's item" }));
  }
  throw new Error(`${who.name} was never handed the item`);
}
async function seed(id, title, tier = 1) {
  await personRecord("tok-gab", { kind: "imported", item: id, source: "check", upstream_id: id, title });
  await personRecord("tok-gab", { kind: "accepted", item: id, tier });
}
async function ingest(record) {
  const response = await fetch(`${TRACKER_URL}/v1/ingest/records`, {
    method: "POST",
    headers: { "content-type": "application/json", authorization: "Bearer tok-ing" },
    body: JSON.stringify({ record }),
  });
  if (!response.ok) throw new Error(`ingest ${record.kind}: ${await response.text()}`);
}

// Names carry a tag because other checks share the log.
const tag = Date.now().toString(36);
const id = (name) => `bk-${tag}-${name}`;

try {
  // 1. Fill the log.
  const felix = await felixToken();
  createRun(newRunId("backup"), "backup check");
  must(["org", "init", "--tracker-subject", "agents-admin"]);
  let tracker = await startTracker(tokens, { ...timing, ...orgEnv });
  const run = newRunId("backupagents");
  createRun(run, "backup check agents");
  const agent = (name) =>
    spawnAgent(felix, run, name, { ...timing, KBL_TRACKER_URL: TRACKER_URL, KBL_TRACKER_TOKEN: `tok-${name}` });
  const [w1, w2, w3] = await Promise.all([agent("w1"), agent("w2"), agent("w3")]);

  // Prices need the newest format; a log that is already there is left as it is.
  await personRecord("tok-gab", { kind: "format_raised", to: 9 }).catch((e) => console.log(`format not raised: ${e.message}`));
  must(["prices", "import", "--default"], { ...admin, KBL_TEAM: DEFAULT });

  // A claim with notes, children, a decision, a step, a progress note and usage.
  await seed(id("root"), "stale read after restart");
  const mine = id("root");
  await take(w1, (i) => i.id === mine);
  await w1.tool("work_context", { entry: "repro", text: "restart between fill and read" });
  const child = await w1.tool("work_post", { key: `reader-${tag}`, work: "split", title: "fix the reader" });
  await w1.tool("decide", { phase: "cause", so_far: "the read beats the fill", why: "the writer holds the lock", commit: "abc123" });
  await w1.tool("record_step", { command: "cargo test", exit_code: 0, output: "ok" });
  await w1.tool("work_progress", { note: "reader fixed", branch: "fix/reader", pr: PR });
  const proposal = await w1.tool("work_post", { key: `found-${tag}`, work: "proposal", title: "the writer leaks a handle" });
  const kid = await take(w2, (i) => i.id === child.item);
  await w2.tool("work_complete", { item: kid.item.id, usage: [{ model: "claude-sonnet-5-5", unit: "tokens", input: 4000, output: 900 }] });
  await w1.tool("work_complete", { item: mine, usage: [{ model: "claude-opus-5-5", unit: "tokens", input: 12000, output: 3000 }] });

  // A person edits and accepts a proposal, and the agent that takes it asks and is answered.
  await personCall("tok-gab", "/v1/work/proposals/edit", { item: proposal.item, title: "the writer leaks a file handle", tier: 3 });
  await personCall("tok-gab", "/v1/work/proposals/accept", { item: proposal.item });
  const asked = await take(w3, (i) => i.id === proposal.item);
  await w3.tool("work_ask", { question: "which handle?", options: ["read", "write"], default: "read" });
  await personCall("tok-gab", "/v1/work/questions/answer", { item: asked.item.id, chosen: "write" });
  // The answer ended w3's hold; its own next call finds that out and drops its local claim.
  await w3.tool("work_complete", { item: asked.item.id }).catch(() => {});

  // A pull request, reviewed: findings in each verdict, then a person's item left ready.
  await ingest({ kind: "pr_status", item: mine, status: { pr: PR, phase: "open", review: "unreviewed", checks: "green" } });
  // Not w2: it worked under the request's item.
  const review = await take(w3, (i) => i.pr === PR, { review_tiers: ["fast"] });
  const f = (verdict, title) => ({ severity: "high", file: "src/read.rs", line: 88, title, scenario: "restart returns the old value", verdict });
  await w3.tool("work_finding", f("defer", "cache has no size bound"));
  await w3.tool("work_finding", f("dismiss", "style nit"));
  await w3.tool("work_complete", { item: review.item.id });

  // Left open on purpose: an accepted item nobody claimed.
  const waiting = id("waiting");
  await seed(waiting, "left for after the restore");

  // A second team with an agent token and a record of its own.
  await teamCreate("alpha");
  const botToken = must(["agent", "add", "bot", "--team", "alpha"], admin).split("\n")[0];
  check(botToken.length > 20, "the tracker minted alpha's agent token");
  const alphaItem = id("alpha");
  const posted = await fetch(`${TRACKER_URL}/v1/teams/alpha/work/records`, {
    method: "POST",
    headers: { "content-type": "application/json", authorization: "Bearer tok-gab" },
    body: JSON.stringify({ record: { kind: "imported", item: alphaItem, source: "check", upstream_id: alphaItem, title: "alpha's item" } }),
  });
  check(posted.ok, "a person records work in alpha");
  const accepted = await fetch(`${TRACKER_URL}/v1/teams/alpha/work/records`, {
    method: "POST",
    headers: { "content-type": "application/json", authorization: "Bearer tok-gab" },
    body: JSON.stringify({ record: { kind: "accepted", item: alphaItem, tier: 1 } }),
  });
  check(accepted.ok, "alpha's item is accepted");

  w1.kill(); w2.kill(); w3.kill();
  tracker.kill();
  await tracker.exited;

  const kinds = new Set(workLog().map((r) => r.kind));
  for (const kind of ["imported", "accepted", "claimed", "context", "proposed", "decision", "step", "progress", "done", "question", "answer", "finding", "price_set"]) {
    check(kinds.has(kind), `the log has ${kind} records`);
  }

  // 2. Back up.
  const orgFile = join(dir, "org.backup");
  const alphaFile = join(dir, "alpha.backup");
  must(["work", "export", file]);
  must(["org", "export", orgFile]);
  must(["work", "export", alphaFile], ALPHA);
  // The digests cover what the files hold, not anything written since.
  const before = digest(join(dir, "before.json"), file);
  const orgBefore = orgDigest(join(dir, "org-before.json"), orgFile);
  const alphaBefore = JSON.parse(must(["work", "digest", "--state", join(dir, "alpha-before.json"), "--backup", alphaFile], ALPHA));
  check(before.records > 20, `the backup holds ${before.records} records`);
  check(orgBefore.records >= 3, `the org backup holds ${orgBefore.records} records`);
  check((statSync(file).mode & 0o777) === 0o600, "a backup file is readable by its owner only");
  const original = workLog().map((r) => JSON.stringify(r));

  // 3. A fresh cluster, and the restore.
  const up = spawnSync("dev/up.sh", { stdio: "inherit" });
  if (up.status !== 0) throw new Error("dev/up.sh failed");
  await felixToken(); // The new tenant answers.
  must(["org", "init", "--tracker-subject", "agents-admin"]);
  // A broker that has just started may not hold its shards' leases yet.
  for (let tries = 1; ; tries++) {
    try {
      tracker = await startTracker(tokens, { ...timing, ...orgEnv });
      break;
    } catch (e) {
      if (tries === 6) throw e;
      await sleep(10000);
    }
  }
  tracker.kill();
  await tracker.exited;
  check(digest(join(dir, "empty.json")).records === 0, "the fresh cluster's log is empty");

  const bad = join(dir, "damaged.backup");
  writeFileSync(bad, readFileSync(file, "utf8").replace('"kind":"done"', '"kind":"gone"'));
  check(!tool(["work", "import", bad]).ok, "a damaged backup is refused");
  check(digest(join(dir, "empty.json")).records === 0, "the refused import wrote nothing");

  // The org log goes in with the tracker stopped, then a tracker that reads it
  // is given alpha's namespace and grants (the team already exists in the log).
  must(["org", "import", orgFile]);
  const orgAfter = orgDigest(join(dir, "org-after.json"));
  check(orgAfter.sha256 === orgBefore.sha256, "the restored org log's digest equals the original's");
  tracker = await startTracker(tokens, { ...timing, ...orgEnv });
  check(/already exists/.test(await teamCreate("alpha")), "alpha is in the restored org log");
  tracker.kill();
  await tracker.exited;
  must(["work", "import", alphaFile], ALPHA);
  check(
    JSON.parse(must(["work", "digest", "--state", join(dir, "alpha-after.json")], ALPHA)).sha256 === alphaBefore.sha256,
    "the restored alpha log's digest equals the original's",
  );

  must(["work", "import", file]);
  const after = digest(join(dir, "after.json"));
  check(after.records === before.records, "every record came back");
  check(workLog().map((r) => JSON.stringify(r)).join("\n") === original.join("\n"), "the restored log is byte for byte the original");
  check(after.sha256 === before.sha256, "the restored fold's digest equals the original's");
  check(readFileSync(join(dir, "after.json"), "utf8") === readFileSync(join(dir, "before.json"), "utf8"), "the canonical states are equal");
  check(/already holds/.test(must(["work", "import", file])), "a second import of the same backup changes nothing");
  check(digest(join(dir, "again.json")).sha256 === before.sha256, "and the log is as it was");
  check(!tool(["work", "import", orgFile]).ok, "another log's backup is refused");

  // 4. The restored log serves.
  tracker = await startTracker(tokens, { ...timing, ...orgEnv });
  const felix2 = await felixToken();
  createRun(newRunId("backupagain"), "backup check, restored");
  const run2 = newRunId("backupagain2");
  createRun(run2, "restored agents");
  const w4 = await spawnAgent(felix2, run2, "w1", { ...timing, KBL_TRACKER_URL: TRACKER_URL, KBL_TRACKER_TOKEN: "tok-w1" });
  const handed = await take(w4, (i) => i.id === waiting);
  check(handed.claimed, "the item that was waiting is handed out after the restore");
  await w4.tool("work_complete", { item: waiting });
  const done = workLog().filter((r) => r.item === mine && r.kind === "done");
  check(done.length === 1, "an item finished before the backup is not done again");
  // Team mode: the token minted before the backup still works, and alpha's item is handed out.
  const bot = await alphaApi(botToken, "next", { wait_seconds: 3 });
  check(bot.status === 200 && bot.body.claimed && bot.body.item?.id === alphaItem, `alpha's restored token is handed alpha's item (${bot.status})`);
  void tracker;
} finally {
  killAll();
}
