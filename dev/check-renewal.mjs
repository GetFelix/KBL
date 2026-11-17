// A worker that renews its Felix token from an ID token file keeps a claim it
// holds across the renewals, and finishes the task. A stand-in in front of the
// control plane counts the exchanges, and KBL_FELIX_TOKEN_MAX_AGE_MS makes
// kbl-mcp renew every second instead of every few hours.
//
//   cargo build -p kbl-mcp -p kbl-tool && node dev/check-renewal.mjs
import { mkdtempSync, writeFileSync } from "node:fs";
import { createServer } from "node:http";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { agent as spawnAgent, check, createRun, felixToken, giveUpAfter, idToken, killAll, newRunId, sleep } from "./lib/stack.mjs";

const RUN = newRunId("renewal");
const CONTROL_PLANE = process.env.KBL_FELIX_CONTROL_PLANE || "http://127.0.0.1:8443";

giveUpAfter(90, "the renewal check");

let exchanges = 0;
const counter = createServer(async (req, res) => {
  exchanges++;
  const body = [];
  for await (const chunk of req) body.push(chunk);
  const upstream = await fetch(`${CONTROL_PLANE}${req.url}`, {
    method: req.method,
    headers: { "content-type": "application/json", authorization: req.headers.authorization },
    body: Buffer.concat(body),
  });
  res.writeHead(upstream.status, { "content-type": "application/json" });
  res.end(await upstream.text());
});
await new Promise((resolve) => counter.listen(0, "127.0.0.1", resolve));

try {
  createRun(RUN, "renewal check");
  const planner = await spawnAgent(await felixToken(), RUN, "planner");
  await planner.tool("task_post", { title: "long job", body: "outlives several renewals", id: "long" });

  const dir = mkdtempSync(join(tmpdir(), "kbl-renewal-"));
  const file = join(dir, "id-token");
  writeFileSync(file, await idToken(), { mode: 0o600 });
  const worker = await spawnAgent("", RUN, "w1", {
    KBL_FELIX_TOKEN: "",
    KBL_FELIX_ID_TOKEN_FILE: file,
    KBL_FELIX_CONTROL_PLANE: `http://127.0.0.1:${counter.address().port}`,
    KBL_FELIX_TOKEN_MAX_AGE_MS: "1000",
  });
  const claim = await worker.tool("task_claim", { wait_seconds: 5 });
  check(claim.claimed && claim.task.id === "long", "the worker claimed the task with a renewed token");

  // Each call opens a stream, which asks for the token and so drives a renewal.
  const before = exchanges;
  for (let n = 0; n < 5; n++) {
    await sleep(1200);
    await worker.tool("task_claim", { wait_seconds: 1 });
  }
  check(exchanges - before >= 2, `the token was renewed ${exchanges - before} times while the claim was held`);

  const done = await worker.tool("task_complete", { task: "long", result: "finished after renewals" });
  check(done.status === "done" && !done.warning, "the claim still stood: the result landed without a lapse warning");
  planner.kill();
  worker.kill();
} finally {
  counter.close();
  killAll();
}
