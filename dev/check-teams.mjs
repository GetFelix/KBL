// Checks teams against the dev stack: the organization is set up, teams are
// created with no tracker restart, a team's records and Felix grants stay in
// that team, and teams survive a restart because they come from org.events.
//
//   cargo build -p kbl-tool -p kbl-tracker --example raw_felix && node dev/check-teams.mjs
import { spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import { check, giveUpAfter, killAll, sleep, startTracker, TRACKER_URL } from "./lib/stack.mjs";

const env = (name, fallback) => process.env[name] || fallback;
const CONTROL_PLANE = env("KBL_FELIX_CONTROL_PLANE", "http://127.0.0.1:8443");
const IDP = env("KBL_IDP", "http://127.0.0.1:9400");
const TOOL = env("KBL_TOOL", "target/debug/kbl-tool");
const RAW = env("KBL_RAW_FELIX", "target/debug/examples/raw_felix");
const TENANT = "agents";
const tenantUrl = `${CONTROL_PLANE}/v1/tenants/${TENANT}`;
const tokens = { people: { "tok-ops": "ops" }, agents: { "tok-w1": "w1" } };
const trackerEnv = { KBL_ORG_NAMESPACE: "kbl" };

giveUpAfter(180, "the teams check");

const run = (bin, args, extraEnv = {}) =>
  spawnSync(bin, args, { encoding: "utf8", env: { ...process.env, ...extraEnv } });
const ops = { KBL_TRACKER_TOKEN: "tok-ops" };

async function api(method, path, token, body) {
  const response = await fetch(`${TRACKER_URL}${path}`, {
    method,
    headers: { "content-type": "application/json", ...(token ? { authorization: `Bearer ${token}` } : {}) },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  return { status: response.status, body: await response.json().catch(() => ({})) };
}
const idToken = async (sub) => (await (await fetch(`${IDP}/token?sub=${sub}&aud=kbl`)).json()).id_token;
const exchange = async (sub, body) =>
  (
    await (
      await fetch(`${tenantUrl}/token/exchange`, {
        method: "POST",
        headers: { "content-type": "application/json", authorization: `Bearer ${await idToken(sub)}` },
        body: JSON.stringify(body),
      })
    ).json()
  ).felix_token;
const listTeams = async () => {
  const { body } = await api("GET", "/v1/org/teams", "tok-ops");
  return Array.isArray(body) ? body : (body.teams ?? []);
};
// Waits a few seconds for each of `names` to be listed and serving.
async function allServing(names) {
  for (let tries = 0; tries < 20; tries++) {
    const teams = await listTeams();
    if (names.every((n) => teams.some((t) => t.team === n && t.serving))) return teams;
    await sleep(500);
  }
  return listTeams();
}
const serves = (teams, name) => teams.some((t) => t.team === name && t.serving);

const init = run(TOOL, ["org", "init", "--tracker-subject", "agents-admin"]);
check(init.status === 0, `org init succeeds ${init.stderr}`);

let tracker = await startTracker(tokens, trackerEnv);
let teams = await allServing(["default"]);
check(
  teams.some((t) => t.team === "default" && t.adopted && t.serving),
  "the stack's first team is listed as adopted and serving",
);

for (const team of ["alpha", "beta"]) {
  const made = run(TOOL, ["team", "create", team], ops);
  check(made.status === 0, `team create ${team} succeeds ${made.stderr}`);
}
teams = await allServing(["alpha", "beta"]);
check(serves(teams, "alpha") && serves(teams, "beta"), "both new teams are serving with no restart");

const again = run(TOOL, ["team", "create", "alpha"], ops);
check(again.status === 0 && /already exists/.test(again.stdout), "creating a team again says it exists");
const direct = await api("POST", "/v1/org/teams", "tok-ops", { team: "alpha" });
check(direct.status === 200 && direct.body.created === false, "the tracker reports created: false the second time");

check((await api("POST", "/v1/org/teams", "tok-w1", { team: "gamma" })).status === 403, "an agent cannot create a team");
check((await api("POST", "/v1/org/teams", null, { team: "gamma" })).status === 401, "no token is refused");
check((await api("POST", "/v1/org/teams", "tok-ops", { team: "kbl" })).status === 400, "the organization's name is not a team");
const refused = run(TOOL, ["team", "create", "kbl"], ops);
check(refused.status !== 0, "kbl-tool refuses the organization's name too");

const item = `teams-${Date.now().toString(36)}`;
const posted = await api("POST", "/v1/teams/alpha/work/records", "tok-ops", {
  record: { kind: "imported", item, source: "gh", upstream_id: item, title: "t" },
});
check(posted.status === 200, `a person records work in alpha ${JSON.stringify(posted.body)}`);
const log = (team) => run(TOOL, ["work", "log"], { KBL_NAMESPACE: team }).stdout;
check(log("alpha").includes(item), "alpha's log has the item");
check(!log("beta").includes(item), "beta's log does not");
const next = await api("POST", "/v1/teams/alpha/work/next", "tok-w1", {});
check(next.status === 404 && next.body.why === "unknown_team", "an agent of default cannot take work from alpha");

const principal = (sub) => createHash("sha256").update(`${IDP}|${sub}`).digest("hex");
const admin = await exchange("agents-admin", { audience: "felix-controlplane" });
const grant = await fetch(`${tenantUrl}/rbac/groupings`, {
  method: "POST",
  headers: { "content-type": "application/json", authorization: `Bearer ${admin}` },
  body: JSON.stringify({ user: principal("alice"), role: "role:kbl-alpha-viewer" }),
});
check(grant.ok || grant.status === 409, "alice is made a viewer of alpha");
const raw = (token, ...args) => run(RAW, args, { FELIX_TOKEN: token });
// A new stream's shard can take a moment to open, so give the one allowed read a few tries.
async function rawRead(token, team) {
  let result;
  for (let tries = 0; tries < 10; tries++) {
    result = raw(token, "read", team, "work.events");
    if (result.status === 0) break;
    await sleep(1000);
  }
  return result;
}
const alice = await exchange("alice", { audience: "felix-broker" });
check((await rawRead(alice, "alpha")).stdout.includes(item), "alice reads alpha's log straight from Felix");
check(raw(alice, "read", "beta", "work.events").status !== 0, "alice cannot read beta's log");
check(raw(alice, "publish", "beta", "work.events", "{}").status !== 0, "alice cannot publish to beta");
check(raw(alice, "publish", "alpha", "work.events", "{}").status !== 0, "a viewer cannot publish to alpha");

const narrowed = await exchange("agents-admin", { audience: "felix-broker", resources: ["namespace:agents/alpha"] });
check((await rawRead(narrowed, "alpha")).status === 0, "a token narrowed to alpha reads alpha");
check(raw(narrowed, "read", "beta", "work.events").status !== 0, "a token narrowed to alpha cannot read beta");
check(raw(narrowed, "publish", "beta", "work.events", "{}").status !== 0, "a token narrowed to alpha cannot publish to beta");

tracker.kill();
await tracker.exited;
tracker = await startTracker(tokens, trackerEnv);
teams = await allServing(["default", "alpha", "beta"]);
check(
  ["default", "alpha", "beta"].every((n) => serves(teams, n)),
  "after a restart the teams come back from org.events",
);

killAll();
