// Runs supervise end to end against the dev stack with the fake profile:
//   1. a supervisor's kbl-mcp starts a pool of 2 fake workers on a milestone
//      that holds 4 items; each worker is its own kbl-mcp with its own token;
//   2. the top-up replaces workers as they finish until every item is done;
//   3. every completion on the log is by a worker, carrying the supervisor;
//   4. each worker reports it is done with worker_finish, so the tracker logs
//      worker_finished for it (format 24) and the top-up sees it ended;
//   5. supervise_stop all leaves no live worker, and a stopped worker's token
//      is refused.
//
//   cargo build -p kbl-mcp -p kbl-tool -p kbl-tracker && node dev/check-supervise.mjs
import { mkdtempSync, readdirSync, readFileSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";

import {
  agent as spawnAgent,
  check,
  createRun,
  felixToken,
  giveUpAfter,
  killAll,
  newRunId,
  personRecord,
  raiseFormat,
  sleep,
  startTracker,
  TRACKER_URL,
  workLog,
} from "./lib/stack.mjs";

const RUN = newRunId("supervise");
const MILESTONE = `supervise-${Date.now()}`;
const ITEMS = ["s1", "s2", "s3", "s4"].map((i) => `${i}-${Date.now()}`);
const tokens = { agents: { "tok-sup": "sup" }, people: { "tok-gab": "gab" } };

giveUpAfter(240, "the supervise check");

const dir = mkdtempSync(join(tmpdir(), "supervise-"));
const configPath = join(dir, "supervise.toml");
writeFileSync(
  configPath,
  `state_dir = ${JSON.stringify(join(dir, "state"))}
kbl_mcp = ${JSON.stringify(resolve(process.env.KBL_MCP ?? "target/debug/kbl-mcp"))}
default_profile = "fake"
top_up_seconds = 5
min_run_seconds = 0
share_felix_token = true

[profiles.fake]
kind = "command"
vendor = "fake"
model = "scripted"
mcp_config = "json"
pass_env = ["PATH"]
set_env = { FAKE_WORKER_CONFIG_ROOT = ${JSON.stringify(join(dir, "state"))}, FAKE_WORKER_FINISH = "1" }
command = [${JSON.stringify(process.execPath)}, ${JSON.stringify(resolve("dev/fake-worker.mjs"))}, "{mcp_config_path}"]
`,
  { mode: 0o600 },
);

const felix = await felixToken();
createRun(RUN, "supervise check");
// A log that holds nothing yet starts at the current format, so supervise needs no raise by hand.
const fresh = workLog().length === 0;
const tracker = await startTracker(tokens, { KBL_CLAIM_VISIBILITY_MS: "8000", KBL_FRESH_LOG_FORMAT: "current" });
try {
  if (fresh) {
    const at = workLog().reduce((n, r) => (r.kind === "format_raised" ? Math.max(n, r.to) : n), 1);
    check(at >= 24, `a new log starts at the current format (it is at ${at}), with no format_raised by hand`);
  } else {
    await raiseFormat("tok-gab", 24).catch((e) => console.log(`format not raised: ${e.message}`));
  }
  for (const item of ITEMS) {
    await personRecord("tok-gab", {
      kind: "imported",
      item,
      source: "check",
      upstream_id: item,
      title: `item ${item}`,
      milestone: MILESTONE,
    });
    await personRecord("tok-gab", { kind: "accepted", item, tier: 2 });
  }

  const sup = await spawnAgent(felix, RUN, "sup", {
    KBL_TRACKER_URL: TRACKER_URL,
    KBL_TRACKER_TOKEN: "tok-sup",
    KBL_SUPERVISE_CONFIG: configPath,
    KBL_EXTEND_EVERY_MS: "1000",
  });
  const started = await sup.tool("supervise_start", { count: 2, milestone: MILESTONE });
  check(started.started.length === 2, `two workers started: ${JSON.stringify(started.started)}`);

  const done = () => {
    const completed = workLog().filter((r) => r.kind === "done" && ITEMS.includes(r.item));
    return completed;
  };
  const finished = [];
  for (let tries = 0; done().length < ITEMS.length; tries++) {
    if (tries > 90) throw new Error(`only ${done().length} of ${ITEMS.length} items completed`);
    const status = await sup.tool("supervise_status", {});
    finished.push(...status.finished);
    if (status.failed.length) {
      for (const f of readdirSync(join(dir, "state"), { recursive: true }).filter((f) => String(f).endsWith("out.log"))) {
        console.log(`--- ${f}\n${readFileSync(join(dir, "state", String(f)), "utf8")}`);
      }
      throw new Error(`a worker failed: ${JSON.stringify(status.failed)}`);
    }
    await sleep(2000);
  }
  const completed = done();
  check(completed.length === ITEMS.length, "every item was completed");
  check(
    completed.every((r) => r.by.startsWith("sup-w") && r.sup === "sup"),
    `each completion names a worker and its supervisor: ${JSON.stringify(completed.map((r) => [r.by, r.sup]))}`,
  );
  const starts = workLog().filter((r) => r.kind === "worker_started" && r.by === "sup");
  check(
    starts.every((r) => r.vendor === "fake" && r.profile === "fake" && r.model === "scripted"),
    "each worker records its vendor, profile and model",
  );
  check(starts.every((r) => !JSON.stringify(r).includes("kblw_")), "no worker token is on the log");

  // A worker reports done only after its queue stays empty for two waits, which is after the last item is done.
  const finishes = () => workLog().filter((r) => r.kind === "worker_finished");
  const starts_n = workLog().filter((r) => r.kind === "worker_started" && r.by === "sup").length;
  for (let waited = 0; finishes().length < starts_n && waited < 30; waited++) await sleep(1000);
  const ended = finishes();
  check(
    ended.length === starts_n && ended.every((r) => r.by.startsWith("sup-w") && r.sup === "sup" && r.actor === "agent"),
    `each worker that ran out of work logged worker_finished for itself: ${JSON.stringify(ended.map((r) => [r.by, r.sup]))}`,
  );
  finished.push(...(await sup.tool("supervise_status", {})).finished);
  check(finished.length >= ended.length, `the supervisor saw the finished workers: ${JSON.stringify(finished)}`);

  const stopped = await sup.tool("supervise_stop", { all: true });
  check(stopped.stop_pending.length === 0, "supervise_stop all stopped every worker");
  const listed = await fetch(`${TRACKER_URL}/v1/work/workers`, { headers: { authorization: "Bearer tok-sup" } }).then((r) =>
    r.json(),
  );
  check(
    listed.workers.every((w) => w.stopped_ms !== null && w.stopped_ms !== undefined),
    "the tracker lists no live worker",
  );
  check(
    listed.workers.some((w) => w.finished === true),
    "the tracker lists the workers that reported done as finished",
  );
  const left = readdirSync(join(dir, "state", "sup"), { recursive: true }).filter((f) => String(f).endsWith("mcp.json"));
  check(left.length === 0, "no worker's MCP config is left on disk");
  console.log("supervise check passed");
} finally {
  killAll();
  tracker.kill();
}
