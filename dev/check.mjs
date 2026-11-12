// Checks that the stack carries what a run needs: the IdP is trusted, the
// control plane creates a run's log and grants a viewer, and the gateway lets
// that viewer subscribe to it while keeping out a run it was not granted.
// It publishes nothing: the gateway's scope is read only.
//
//   node dev/check.mjs
import { createHash } from "node:crypto";

const env = (name, fallback) => process.env[name] || fallback;
const CONTROL_PLANE = env("CONTROL_PLANE_URL", "http://127.0.0.1:8443");
const GATEWAY = env("GATEWAY_URL", "ws://127.0.0.1:8787/ws");
const IDP = env("IDP_URL", "http://127.0.0.1:9400");
const TENANT = "agents";
const AUDIENCE = "kbl";
const RUN = "check";

const principal = (subject) => createHash("sha256").update(`${IDP}|${subject}`).digest("hex");
const object = (name) => `stream:${TENANT}/default/${name}`;

async function request(method, url, { token, body } = {}) {
  const response = await fetch(url, {
    method,
    headers: { "content-type": "application/json", ...(token ? { authorization: `Bearer ${token}` } : {}) },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  const text = await response.text();
  if (response.status === 409) return null;
  if (!response.ok) throw new Error(`${method} ${url} -> ${response.status}: ${text}`);
  return text ? JSON.parse(text) : null;
}

const idToken = async (sub) => (await request("GET", `${IDP}/token?sub=${sub}&aud=${AUDIENCE}`)).id_token;

const admin = (
  await request("POST", `${CONTROL_PLANE}/v1/tenants/${TENANT}/token/exchange`, {
    token: await idToken("agents-admin"),
    body: { audience: "felix-controlplane" },
  })
).felix_token;
console.log("ok   the control plane exchanged the admin's token");

const tenant = `${CONTROL_PLANE}/v1/tenants/${TENANT}`;
const createLog = (run) =>
  request("POST", `${tenant}/namespaces/default/streams`, {
    token: admin,
    body: {
      stream: `agents.log.${run}`,
      kind: "Stream",
      shards: 1,
      replication_factor: 1,
      retention: { max_age_seconds: null, max_size_bytes: null },
      consistency: "Leader",
      delivery: "AtLeastOnce",
      durable: true,
    },
  });
await createLog(RUN);
const role = `role:agents-${RUN}-watch`;
await request("POST", `${tenant}/rbac/policies`, {
  token: admin,
  body: { subject: role, object: object(`agents.log.${RUN}`), action: "stream.subscribe" },
});
await request("POST", `${tenant}/rbac/groupings`, { token: admin, body: { user: principal("viewer"), role } });
console.log(`ok   created agents.log.${RUN} and let the viewer follow it`);

const viewer = await idToken("viewer");

// Joins `run` and follows its log live; resolves with the first message that settles it.
function follow(run) {
  return new Promise((resolve, reject) => {
    const ws = new WebSocket(GATEWAY);
    const timer = setTimeout(() => finish(reject, new Error(`${run}: no answer in 15s`)), 15_000);
    const finish = (fn, value) => {
      clearTimeout(timer);
      ws.onclose = null;
      ws.close();
      fn(value);
    };
    ws.onerror = () => finish(reject, new Error(`${run}: websocket error`));
    ws.onclose = () => finish(reject, new Error(`${run}: closed early`));
    ws.onopen = () => ws.send(JSON.stringify({ type: "join", protocol: 1, run, token: viewer }));
    ws.onmessage = ({ data }) => {
      const message = JSON.parse(data);
      if (message.type === "hello") ws.send(JSON.stringify({ type: "subscribe", stream: "log", from: "live" }));
      else if (message.type === "subscribed" || message.type === "error") finish(resolve, message);
    };
  });
}

// A new stream's shard takes a moment to be assigned to, and opened by, the broker.
let granted = await follow(RUN);
for (let attempt = 1; attempt < 30 && granted.code === "subscribe_failed"; attempt++) {
  await new Promise((resolve) => setTimeout(resolve, 1000));
  granted = await follow(RUN);
}
if (granted.type !== "subscribed") throw new Error(`the viewer could not follow its run: ${JSON.stringify(granted)}`);
console.log("ok   the gateway let the viewer subscribe to its run");

// The stream exists, so the refusal is about the grant and not about a missing run.
await createLog("other");
const refused = await follow("other");
if (refused.code !== "forbidden") throw new Error(`expected the join to be forbidden, got ${JSON.stringify(refused)}`);
console.log("ok   the gateway kept the viewer out of a run it was not granted");
