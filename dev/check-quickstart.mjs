// Checks the quick start stack the way its README uses it: the tracker answers,
// the dashboard serves its page and every script and wasm file the page loads,
// and forwards /v1, then the README's own commands run as written: they add an
// item as a person and write an .mcp.json, and kbl-mcp started from that file
// claims the item. The optional ingest step runs against a stand-in GitHub,
// since CI has no real repository or token.
//
//   KBL_QUICKSTART=1 dev/up.sh && cargo build --release -p kbl-mcp -p kbl-ingest && node dev/check-quickstart.mjs
import { execFileSync, spawn } from "node:child_process";
import { createServer } from "node:http";
import { readFileSync, statSync, rmSync } from "node:fs";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { agent, check, giveUpAfter, killAll } from "./lib/stack.mjs";

giveUpAfter(240, "the quick start check");

const TRACKER = "http://127.0.0.1:8790";
const DASHBOARD = "http://127.0.0.1:8081";
const tokens = JSON.parse(readFileSync(new URL("./state/tracker-tokens.json", import.meta.url), "utf8"));
const me = Object.keys(tokens.people)[0];
const repo = fileURLToPath(new URL("..", import.meta.url));

// The fenced bash blocks of README.md's Quick start section that touch `marker`,
// minus lines that start an interactive program, run from the repository root.
function readmeBlock(marker) {
  const readme = readFileSync(new URL("../README.md", import.meta.url), "utf8");
  const section = readme.split("\n## Quick start")[1]?.split("\n## ")[0] ?? "";
  const block = [...section.matchAll(/```bash\n([\s\S]*?)```/g)].map((m) => m[1]).find((b) => b.includes(marker));
  check(block, `the README quick start has a command block with ${marker}`);
  return block.split("\n").filter((l) => !l.trim().startsWith("claude ")).join("\n");
}
const runReadme = (marker) => execFileSync("bash", ["-eo", "pipefail", "-c", readmeBlock(marker)], { cwd: repo, encoding: "utf8" });

const get = (url, token) => fetch(url, token ? { headers: { authorization: `Bearer ${token}` } } : {});

try {
  check((await get(`${TRACKER}/healthz`)).ok, "the tracker answers /healthz");
  check((await get(`${TRACKER}/v1/me`)).status === 401, "the tracker refuses a call with no token");
  check((await get(`${TRACKER}/v1/me`, me)).ok, "the tracker knows the person token");
  const page = await get(`${DASHBOARD}/`);
  const html = await page.text();
  check(page.ok && html.includes("<html"), "the dashboard serves its page");
  // The page is useless if its script or wasm does not load, so fetch what it references.
  const refs = [...html.matchAll(/<(?:script|link)\b[^>]*?(?:src|href)="([^"]+)"/g)]
    .map((m) => new URL(m[1], `${DASHBOARD}/`))
    .filter((u) => u.origin === DASHBOARD && /\.(js|mjs|css|wasm)$/.test(u.pathname));
  check(refs.some((u) => /\.m?js$/.test(u.pathname)), "the dashboard page references a script");
  const seen = new Set();
  for (const url of refs) {
    if (seen.has(url.href)) continue;
    seen.add(url.href);
    const asset = await get(url.href);
    const body = Buffer.from(await asset.arrayBuffer());
    check(asset.ok && body.length > 0, `the dashboard serves ${url.pathname}`);
    if (/\.m?js$/.test(url.pathname)) {
      for (const [, ref] of body.toString("utf8").matchAll(/["'`]([\w./-]+\.wasm)["'`]/g)) {
        const wasm = new URL(ref, url);
        if (seen.has(wasm.href)) continue;
        seen.add(wasm.href);
        const w = await get(wasm.href);
        const bytes = Buffer.from(await w.arrayBuffer());
        check(w.ok && bytes.subarray(0, 4).equals(Buffer.from([0, 0x61, 0x73, 0x6d])), `the dashboard serves ${wasm.pathname} as wasm`);
      }
    }
  }
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

  // README step 2 and step 3, as written.
  runReadme("tracker-tokens.json");
  const item = "demo-1";
  const mcpFile = fileURLToPath(new URL("../.mcp.json", import.meta.url));
  runReadme("dev/connect.mjs");
  check((statSync(mcpFile).mode & 0o777) === 0o600, "the generated config is mode 600");
  const server = JSON.parse(readFileSync(mcpFile, "utf8")).mcpServers;
  const { command, env } = Object.values(server)[0];
  const who = await agent(env.KBL_FELIX_TOKEN, env.KBL_RUN, env.KBL_AGENT, env, command);
  const reply = await who.tool("work_next", { wait_seconds: 10 });
  check(reply.claimed && reply.item.id === item, "kbl-mcp from the generated config claimed the item");
  await who.tool("work_complete", { item });

  // README step 4: ingest, with the check first, against a stand-in GitHub.
  const ghRepo = `check/quickstart-${Date.now().toString(36)}`;
  const issue = JSON.parse(readFileSync(new URL("../ingest/fixtures/issue.json", import.meta.url), "utf8"));
  issue.url = `https://api.github.com/repos/${ghRepo}/issues/812`;
  issue.repository_url = `https://api.github.com/repos/${ghRepo}`;
  const github = createServer((req, res) => {
    const path = new URL(req.url, "http://gh").pathname;
    const empty = path.endsWith("/comments") || path.endsWith("/blocked_by") || path.endsWith("/sub_issues");
    const body = req.method !== "GET" ? null : path === "/user" ? { login: "octocat" } : path === `/repos/${ghRepo}` ? { permissions: { pull: true } } : path === `/repos/${ghRepo}/issues` ? [issue] : path.endsWith("/issues/812") ? issue : empty ? [] : null;
    res.writeHead(body ? 200 : 404, { "content-type": "application/json" });
    res.end(JSON.stringify(body ?? { message: "Not Found" }));
  });
  await new Promise((resolve) => github.listen(0, "127.0.0.1", resolve));
  const ingest = (extra) =>
    new Promise((resolve) => {
      const child = spawn("node", ["dev/ingest.mjs", "--repo", ghRepo, "--github-api", `http://127.0.0.1:${github.address().port}`, ...extra], {
        cwd: repo,
        env: { ...process.env, GITHUB_TOKEN: "unused" },
        stdio: ["ignore", "inherit", "inherit"],
      });
      child.on("exit", resolve);
    });
  check((await ingest(["--check-only"])) === 0, "kbl-ingest check passes against the quick start tracker");
  check((await ingest(["--once"])) === 0, "kbl-ingest imports once into the quick start tracker");
  // Async so the stand-in GitHub on this event loop keeps answering.
  const after = JSON.parse(
    await new Promise((resolve, reject) => {
      const child = spawn(join(repo, "target/release/kbl-ingest"), ["check", "--config", join(repo, "dev/state/ingest.json"), "--json"], {
        env: { ...process.env, GITHUB_TOKEN: "unused", KBL_INGEST_TOKEN: Object.keys(tokens.ingest)[0] },
        stdio: ["ignore", "pipe", "inherit"],
      });
      let out = "";
      child.stdout.on("data", (d) => (out += d));
      child.on("error", reject);
      child.on("exit", (code) => (code === 0 ? resolve(out) : reject(new Error(`kbl-ingest check exited ${code}`))));
    }),
  );
  check(after.rules[0].already_imported === 1, "the issue is a root in the tracker");
  github.close();
} finally {
  rmSync(fileURLToPath(new URL("../.mcp.json", import.meta.url)), { force: true });
  killAll();
}
