// A worker that renews its Felix token from an ID token file outlives the
// token it started with. Run it on a stack whose tokens last 20 seconds, so
// the broker refuses the first token about 80 seconds in (its 60 s leeway):
//
//   KBL_DEV_TOKEN_TTL_SECONDS=20 dev/up.sh
//   cargo build -p kbl-mcp -p kbl-tool && node dev/check-renewal.mjs
//
// A stand-in in front of the control plane records each exchange. The worker
// claims a task and then sits idle past that refusal point. It must have
// renewed on schedule with no traffic driving it (at two thirds of each
// token's life), and then a publish, a cache write and the ack of the claim
// it held all along must succeed.
import { mkdtempSync, renameSync, writeFileSync } from "node:fs";
import { createServer } from "node:http";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { agent as spawnAgent, check, createRunWhenReady, felixToken, giveUpAfter, idToken, killAll, sleep, whenReady } from "./lib/stack.mjs";

const CONTROL_PLANE = process.env.KBL_FELIX_CONTROL_PLANE || "http://127.0.0.1:8443";
// The broker accepts a token this long past its `exp`.
const LEEWAY_MS = 60_000;

giveUpAfter(240, "the renewal check");

// The broker holds its leases with a token of the same short life, and re-reads
// the file it came from. Left alone it would lapse its leases a minute and a
// half in, which is the broker failing, not the worker under test. The seed
// made the state directory writable for the broker's user.
const NODE_TOKEN = join(dirname(fileURLToPath(import.meta.url)), "state", "node.token");
async function renewNodeToken() {
  const exchanged = await fetch(`${CONTROL_PLANE}/v1/tenants/agents/token/exchange`, {
    method: "POST",
    headers: { "content-type": "application/json", authorization: `Bearer ${await idToken("agents-broker")}` },
    body: JSON.stringify({ audience: "felix-controlplane" }),
  });
  if (!exchanged.ok) throw new Error(`the broker's token was not renewed: ${exchanged.status}`);
  const next = `${NODE_TOKEN}.next`;
  writeFileSync(next, (await exchanged.json()).felix_token, { mode: 0o644 });
  renameSync(next, NODE_TOKEN);
}
await renewNodeToken();
const nodeTokenTimer = setInterval(() => renewNodeToken().catch((e) => console.error(e.message)), 5000);

// Arrival time (ms) and `exp` (s) of each exchange the worker made.
const exchanges = [];
const counter = createServer(async (req, res) => {
  const at = Date.now();
  const body = [];
  for await (const chunk of req) body.push(chunk);
  const upstream = await fetch(`${CONTROL_PLANE}${req.url}`, {
    method: req.method,
    headers: { "content-type": "application/json", authorization: req.headers.authorization },
    body: Buffer.concat(body),
  });
  const text = await upstream.text();
  if (upstream.ok) {
    const token = JSON.parse(text).felix_token;
    const { exp } = JSON.parse(Buffer.from(token.split(".")[1], "base64url").toString("utf8"));
    exchanges.push({ at, exp });
  }
  res.writeHead(upstream.status, { "content-type": "application/json" });
  res.end(text);
});
await new Promise((resolve) => counter.listen(0, "127.0.0.1", resolve));

try {
  const RUN = await createRunWhenReady("renewal", "renewal check");
  await whenReady("the first task", async () => {
    const planner = await spawnAgent(await felixToken(), RUN, "planner");
    try {
      await planner.tool("task_post", { title: "long job", body: "outlives the first token", id: "long" });
    } finally {
      planner.kill();
    }
  });

  const dir = mkdtempSync(join(tmpdir(), "kbl-renewal-"));
  const file = join(dir, "id-token");
  writeFileSync(file, await idToken(), { mode: 0o600 });
  const worker = await spawnAgent("", RUN, "w1", {
    KBL_FELIX_TOKEN: "",
    KBL_FELIX_ID_TOKEN_FILE: file,
    KBL_FELIX_CONTROL_PLANE: `http://127.0.0.1:${counter.address().port}`,
  });
  check(exchanges.length === 1, `the worker connected with one exchanged token (${exchanges.length})`);
  const first = exchanges[0];
  const life = first.exp * 1000 - first.at;
  check(life > 5000 && life <= 30000, `the stack's tokens are short (${Math.round(life / 1000)} s); start it with KBL_DEV_TOKEN_TTL_SECONDS=20`);
  const claim = await worker.tool("task_claim", { wait_seconds: 5 });
  check(claim.claimed && claim.task.id === "long", "the worker claimed the task");

  // Idle until the broker would refuse the first token, plus a margin.
  await sleep(first.exp * 1000 + LEEWAY_MS + 5000 - Date.now());

  // Renewal is due at two thirds of a token's life, counted in whole seconds.
  // The claim's keepalive runs on open streams and never asks for a token, so
  // every exchange after the first is the renewal loop's own.
  const gaps = exchanges.slice(1).map((e, i) => e.at - exchanges[i].at);
  const due = (life * 2) / 3;
  check(
    gaps.length >= 4 && gaps.every((g) => g > due - 1500 && g < due + 2500),
    `renewed on schedule while idle, every ${Math.round(due)} ms: ${JSON.stringify(gaps)}`,
  );
  check(
    exchanges.slice(1).every((e, i) => e.at < exchanges[i].exp * 1000),
    "each token was renewed before the one before it expired",
  );

  const posted = await worker.tool("task_post", { title: "after expiry", body: "posted on a renewed token", id: "after" });
  check(posted.task === "after", "a publish after the broker would refuse the first token succeeded");
  // Completing writes the result to the cache and acks the claim held since the start.
  const done = await worker.tool("task_complete", { task: "long", result: "finished after the first token expired" });
  check(done.status === "done" && !done.warning, "the claim held across the renewals completed: its cache write and ack landed with no lapse warning");
  worker.kill();
} finally {
  clearInterval(nodeTokenTimer);
  counter.close();
  killAll();
}
