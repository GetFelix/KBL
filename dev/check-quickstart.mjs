// Checks the quick start stack the way its README uses it: the tracker answers,
// the dashboard serves and forwards /v1, a person adds an item, and kbl-mcp
// started from the .mcp.json that dev/connect.mjs wrote claims it.
//
//   KBL_QUICKSTART=1 dev/up.sh && cargo build -p kbl-mcp && node dev/check-quickstart.mjs
import { readFileSync, statSync, mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { connect } from "./connect.mjs";
import { agent, check, giveUpAfter, killAll } from "./lib/stack.mjs";

giveUpAfter(120, "the quick start check");

const TRACKER = "http://127.0.0.1:8790";
const DASHBOARD = "http://127.0.0.1:8081";
const tokens = JSON.parse(readFileSync(new URL("./state/tracker-tokens.json", import.meta.url), "utf8"));
const me = Object.keys(tokens.people)[0];
const get = (url, token) => fetch(url, token ? { headers: { authorization: `Bearer ${token}` } } : {});
const post = (path, token, body) =>
  fetch(`${TRACKER}${path}`, {
    method: "POST",
    headers: { "content-type": "application/json", authorization: `Bearer ${token}` },
    body: JSON.stringify(body),
  });

try {
  check((await get(`${TRACKER}/healthz`)).ok, "the tracker answers /healthz");
  check((await get(`${TRACKER}/v1/me`)).status === 401, "the tracker refuses a call with no token");
  check((await get(`${TRACKER}/v1/me`, me)).ok, "the tracker knows the person token");
  const page = await get(`${DASHBOARD}/`);
  check(page.ok && (await page.text()).includes("<html"), "the dashboard serves its page");
  check((await get(`${DASHBOARD}/v1/me`, me)).ok, "the dashboard forwards /v1 to the tracker");
  check((await get("http://127.0.0.1:8788/oidc")).ok, "the dashboard's gateway answers");

  // The dashboard's own join: the scope field and stream must match what the page sends.
  const { id_token } = await (await fetch("http://127.0.0.1:9400/token?sub=agents-admin&aud=kbl")).json();
  const hello = await new Promise((resolve, reject) => {
    const ws = new WebSocket("ws://127.0.0.1:8788/ws");
    setTimeout(() => reject(new Error("no hello from the dashboard's gateway")), 15000).unref();
    ws.onopen = () => ws.send(JSON.stringify({ type: "join", protocol: 1, features: ["read"], room: "kbl", token: id_token }));
    ws.onmessage = (e) => { resolve(JSON.parse(e.data)); ws.close(); };
  });
  check(hello.type === "hello", "the dashboard's gateway lets the page join");

  const item = `qs-${Date.now().toString(36)}`;
  for (const record of [
    { kind: "imported", item, source: "quickstart", upstream_id: item, title: "Quick start check" },
    { kind: "accepted", item, tier: 1 },
  ]) {
    const reply = await post("/v1/work/records", me, { record });
    check(reply.ok, `a person wrote ${record.kind}`);
  }

  const dir = mkdtempSync(join(tmpdir(), "qs-"));
  const { out } = await connect(["agent1", "--out", join(dir, ".mcp.json")]);
  check((statSync(out).mode & 0o777) === 0o600, "the generated config is mode 600");
  const server = JSON.parse(readFileSync(out, "utf8")).mcpServers;
  const { command, env } = Object.values(server)[0];
  const who = await agent(env.KBL_FELIX_TOKEN, env.KBL_RUN, env.KBL_AGENT, env, command);
  const reply = await who.tool("work_next", { wait_seconds: 10 });
  check(reply.claimed && reply.item.id === item, "kbl-mcp from the generated config claimed the item");
  await who.tool("work_complete", { item });
} finally {
  killAll();
}
