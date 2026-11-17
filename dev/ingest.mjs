#!/usr/bin/env node
// Brings GitHub issues into the quick start's tracker. It writes an ingest
// config for one repository and label, runs `kbl-ingest check` against it, and
// then runs the worker on the host (ingest reads a token file and talks to the
// tracker on loopback, so it needs no container). See "Quick start" in README.md.
//
//   node dev/ingest.mjs --repo OWNER/NAME [--token-file FILE] [--label agent-ok]
//                       [--once] [--check-only] [--ingest /path/to/kbl-ingest]
//
// Without --token-file the GitHub token is read from $GITHUB_TOKEN.
import { spawn, spawnSync } from "node:child_process";
import { existsSync, readFileSync, writeFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { parseArgs } from "node:util";

const here = dirname(fileURLToPath(import.meta.url));
const repo = resolve(here, "..");

const { values } = parseArgs({
  options: {
    repo: { type: "string" },
    "token-file": { type: "string" },
    label: { type: "string", default: "agent-ok" },
    ingest: { type: "string" },
    // A GitHub Enterprise address, or a stand-in for tests.
    "github-api": { type: "string" },
    once: { type: "boolean", default: false },
    "check-only": { type: "boolean", default: false },
  },
});

function fail(message) {
  console.error(message);
  process.exit(1);
}

if (!/^[\w.-]+\/[\w.-]+$/.test(values.repo ?? "")) fail("pass --repo OWNER/NAME, the GitHub repository to read issues from");
if (!values["token-file"] && !process.env.GITHUB_TOKEN) {
  fail("give a GitHub token: --token-file FILE, or export GITHUB_TOKEN");
}
if (values["token-file"] && !existsSync(values["token-file"])) fail(`token file ${values["token-file"]} does not exist`);

const tokensPath = join(here, "state", "tracker-tokens.json");
if (!existsSync(tokensPath)) fail("the quick start is not running: start it with dev/quickstart.sh");
const ingestToken = Object.keys(JSON.parse(readFileSync(tokensPath, "utf8")).ingest ?? {})[0];
if (!ingestToken) fail("the tracker's token file has no ingest token: restart the stack with dev/quickstart.sh");

const candidates = [values.ingest, process.env.KBL_INGEST, join(repo, "target/release/kbl-ingest"), join(repo, "target/debug/kbl-ingest")];
const bin = candidates.filter(Boolean).map((p) => resolve(p)).find(existsSync);
if (!bin) fail("kbl-ingest is not built: run `cargo build --release -p kbl-ingest`, or pass --ingest <path>");

// The config holds no secret: it names the token file or variable.
const github = values["token-file"]
  ? { token_file: resolve(values["token-file"]) }
  : { token_env: "GITHUB_TOKEN" };
if (values["github-api"]) github.api = values["github-api"];
const config = join(here, "state", "ingest.json");
writeFileSync(
  config,
  `${JSON.stringify(
    {
      tracker: { url: process.env.KBL_TRACKER_URL ?? "http://127.0.0.1:8790", token_env: "KBL_INGEST_TOKEN" },
      github,
      rules: [{ name: values.label, repo: values.repo, mode: "admit", labels_all: [values.label] }],
    },
    null,
    2,
  )}\n`,
);

const env = { ...process.env, KBL_INGEST_TOKEN: ingestToken };
const checked = spawnSync(bin, ["check", "--config", config], { env, stdio: "inherit" });
if (checked.status !== 0) fail("kbl-ingest check found errors; fix them and run this again");
if (values["check-only"]) process.exit(0);

console.log(`ingest is running; issues in ${values.repo} labelled ${values.label} arrive on the dashboard. Ctrl-C stops it.`);
const worker = spawn(bin, ["--config", config, ...(values.once ? ["--once"] : [])], { env, stdio: "inherit" });
worker.on("exit", (code) => process.exit(code ?? 1));
