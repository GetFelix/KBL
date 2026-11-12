// Seeds a Felix for local work: a tenant that trusts idp.mjs, a namespace, and
// the broker's credential. Runs and their agents are created later by
// `agents-tool`, which signs in as `agents-admin`. Safe to run again.
import { createHash } from "node:crypto";
import { chmod, mkdir, writeFile } from "node:fs/promises";

const env = (name, fallback) => process.env[name] || fallback;

const CONTROL_PLANE = env("AGENTS_FELIX_CONTROL_PLANE", "http://controlplane:8443");
const BOOTSTRAP = env("AGENTS_FELIX_BOOTSTRAP", "http://controlplane:9095");
const BOOTSTRAP_TOKEN = env("AGENTS_FELIX_BOOTSTRAP_TOKEN", "dev-bootstrap");
const IDP = env("AGENTS_IDP", "http://idp:9400");
const STATE = env("AGENTS_STATE_DIR", "/state");
const TENANT = env("AGENTS_TENANT", "agents");
const NAMESPACE = env("AGENTS_NAMESPACE", "default");
// The viewer's OIDC client id, and the audience of every ID token.
const AUDIENCE = "kbl";

async function request(method, url, { token, headers = {}, body } = {}) {
  const response = await fetch(url, {
    method,
    headers: {
      "content-type": "application/json",
      ...(token ? { authorization: `Bearer ${token}` } : {}),
      ...headers,
    },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  const text = await response.text();
  if (response.status === 409) {
    console.log(`  ${method} ${url}: already exists`);
    return null;
  }
  if (!response.ok) throw new Error(`${method} ${url} -> ${response.status}: ${text}`);
  return text ? JSON.parse(text) : null;
}

const { issuer } = await request("GET", `${IDP}/.well-known/openid-configuration`);
// Felix keys RBAC on sha256(issuer|subject), not on the subject itself.
const principal = (subject) => createHash("sha256").update(`${issuer}|${subject}`).digest("hex");

async function exchange(subject, body) {
  const { id_token } = await request("GET", `${IDP}/token?sub=${subject}&aud=${AUDIENCE}`);
  const url = `${CONTROL_PLANE}/v1/tenants/${TENANT}/token/exchange`;
  return (await request("POST", url, { token: id_token, body })).felix_token;
}

console.log(`bootstrap tenant ${TENANT}`);
await request("POST", `${BOOTSTRAP}/internal/bootstrap/tenants/${TENANT}/initialize`, {
  headers: { "x-felix-bootstrap-token": BOOTSTRAP_TOKEN },
  body: {
    display_name: "Felix Agents",
    idp_issuers: [
      {
        issuer,
        audiences: [AUDIENCE],
        jwks_url: `${IDP}/jwks.json`,
        claim_mappings: { subject_claim: "sub" },
      },
    ],
    initial_admin_principals: [principal("agents-admin")],
    policies: [
      { subject: "role:broker", object: "cluster:*", action: "node.view" },
      { subject: "role:broker", object: "cluster:*", action: "node.manage" },
    ],
    groupings: [{ user: principal("agents-broker"), role: "role:broker" }],
  },
});

const admin = await exchange("agents-admin", { audience: "felix-controlplane" });
console.log(`namespace ${NAMESPACE}`);
await request("POST", `${CONTROL_PLANE}/v1/tenants/${TENANT}/namespaces`, {
  token: admin,
  body: { namespace: NAMESPACE, display_name: NAMESPACE },
});

// The broker runs as uid 65532 and writes its certificate here too.
await mkdir(STATE, { recursive: true });
await chmod(STATE, 0o777).catch(() => {});
const broker = await exchange("agents-broker", { audience: "felix-controlplane" });
await writeFile(`${STATE}/node.token`, broker, { mode: 0o644 });
console.log(`wrote node.token to ${STATE}`);

// dev/up.sh waits for this before it calls the stack ready.
await writeFile(`${STATE}/seeded`, "");
