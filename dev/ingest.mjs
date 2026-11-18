#!/usr/bin/env node
// Brings GitHub issues into the quick start's tracker. It writes an ingest
// config for one repository and label, runs `kbl-ingest check` against it, and
// then runs the worker on the host (ingest reads a token file and talks to the
// tracker on loopback, so it needs no container). See "Quick start" in README.md.
//
//   node dev/ingest.mjs --repo OWNER/NAME [--token-file FILE] [--labels agent-ok,bug]
//                       [--exclude-label wontfix,blocked] [--milestone NAME] [--cap N]
//                       [--mode admit|propose] [--keep-config] [--once] [--check-only]
//                       [--ingest /path/to/kbl-ingest]
//
// --labels needs every label listed, --exclude-label skips an issue with any of
// its labels, and --cap bounds the open roots the rule may have. --mode propose
// parks matches in Triage until a person accepts them. --keep-config reuses
// dev/state/ingest.json as it is (for a hand-edited config) and ignores the
// filter flags.
//
// Without --token-file the GitHub token is read from $GITHUB_TOKEN.
import { spawn, spawnSync } from "node:child_process";
import { existsSync, readFileSync, writeFileSync } from "node:fs";
import { homedir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { parseArgs } from "node:util";

const here = dirname(fileURLToPath(import.meta.url));
const repo = resolve(here, "..");

const { values } = parseArgs({
  options: {
    repo: { type: "string" },
    "token-file": { type: "string" },
    labels: { type: "string", default: "agent-ok" },
    // The old single-label spelling.
    label: { type: "string" },
    "exclude-label": { type: "string" },
    milestone: { type: "string" },
    cap: { type: "string" },
    mode: { type: "string", default: "admit" },
    "keep-config": { type: "boolean", default: false },
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

const list = (text) => (text ?? "").split(",").map((l) => l.trim()).filter(Boolean);
const labels = list(values.label ?? values.labels);
const excluded = list(values["exclude-label"]);
if (!["admit", "propose"].includes(values.mode)) fail("--mode is admit or propose");
const cap = values.cap === undefined ? undefined : Number(values.cap);
if (cap !== undefined && !(Number.isInteger(cap) && cap > 0)) fail("--cap is a whole number above 0");
const config = join(here, "state", "ingest.json");
if (values["keep-config"] && !existsSync(config)) fail(`--keep-config needs ${config}; run once without it to write one`);

if (!values["keep-config"] && !/^[\w.-]+\/[\w.-]+$/.test(values.repo ?? "")) fail("pass --repo OWNER/NAME, the GitHub repository to read issues from");
if (!values["keep-config"] && !values["token-file"] && !process.env.GITHUB_TOKEN) {
  fail("give a GitHub token: --token-file FILE, or export GITHUB_TOKEN");
}
if (values["token-file"] && !existsSync(values["token-file"])) fail(`token file ${values["token-file"]} does not exist`);

const tokensPath = join(here, "state", "tracker-tokens.json");
if (!existsSync(tokensPath)) fail("the quick start is not running: start it with dev/quickstart.sh");
const ingestToken = Object.keys(JSON.parse(readFileSync(tokensPath, "utf8")).ingest ?? {})[0];
if (!ingestToken) fail("the tracker's token file has no ingest token: restart the stack with dev/quickstart.sh");

const candidates = [
  values.ingest,
  process.env.KBL_INGEST,
  join(repo, "target/release/kbl-ingest"),
  join(repo, "target/debug/kbl-ingest"),
  join(homedir(), ".kbl/bin/kbl-ingest"),
  join(here, "state/bin/kbl-ingest"),
];
const bin = candidates.filter(Boolean).map((p) => resolve(p)).find(existsSync);
if (!bin) fail("kbl-ingest not found: set KBL_INGEST or pass --ingest <path>, put it in ~/.kbl/bin or dev/state/bin, or run `cargo build --release -p kbl-ingest`");

// The config holds no secret: it names the token file or variable.
if (!values["keep-config"]) {
  const github = values["token-file"]
    ? { token_file: resolve(values["token-file"]) }
    : { token_env: "GITHUB_TOKEN" };
  if (values["github-api"]) github.api = values["github-api"];
  const rule = { name: labels.join("+") || values.repo, repo: values.repo, mode: values.mode, labels_all: labels };
  if (excluded.length) rule.labels_excluded = excluded;
  if (values.milestone) rule.milestone = values.milestone;
  if (cap !== undefined) rule.cap = cap;
  writeFileSync(
    config,
    `${JSON.stringify(
      {
        tracker: { url: process.env.KBL_TRACKER_URL ?? "http://127.0.0.1:8790", token_env: "KBL_INGEST_TOKEN" },
        github,
        rules: [rule],
      },
      null,
      2,
    )}\n`,
  );
}

const env = { ...process.env, KBL_INGEST_TOKEN: ingestToken };
const checked = spawnSync(bin, ["check", "--config", config], { env, stdio: "inherit" });
if (checked.status !== 0) fail("kbl-ingest check found errors; fix them and run this again");
if (values["check-only"]) process.exit(0);

console.log(
  values["keep-config"]
    ? `ingest is running with ${config}. Ctrl-C stops it.`
    : `ingest is running; issues in ${values.repo} labelled ${labels.join(", ")} arrive on the dashboard${values.mode === "propose" ? " (in Triage)" : ""}. Ctrl-C stops it.`,
);
const worker = spawn(bin, ["--config", config, ...(values.once ? ["--once"] : [])], { env, stdio: "inherit" });
worker.on("exit", (code) => process.exit(code ?? 1));
