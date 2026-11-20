// node --test dev/lib/binaries.test.mjs
import assert from "node:assert/strict";
import { chmodSync, mkdirSync, mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { delimiter, join, resolve } from "node:path";
import { test } from "node:test";

import { binaryCandidates, findBinary, notFoundMessage } from "./binaries.mjs";
import { buildRule } from "./ingest-rule.mjs";

const scratch = () => mkdtempSync(join(tmpdir(), "binaries-"));
const touch = (path) => (mkdirSync(join(path, ".."), { recursive: true }), writeFileSync(path, "#!/bin/sh\n"), chmodSync(path, 0o755), path);

test("a binary on PATH is found when nothing else has it", () => {
  const dir = scratch();
  const bin = touch(join(dir, "bin", "kbl-ingest"));
  const found = findBinary("kbl-ingest", { pathEnv: ["/nonexistent", join(dir, "bin")].join(delimiter), home: join(dir, "home"), root: join(dir, "repo") });
  assert.equal(found, bin);
});

test("explicit options and the environment variable beat PATH", () => {
  const dir = scratch();
  const onPath = touch(join(dir, "bin", "kbl-ingest"));
  const given = touch(join(dir, "given", "kbl-ingest"));
  const fromEnv = touch(join(dir, "env", "kbl-ingest"));
  const base = { pathEnv: join(dir, "bin"), home: join(dir, "home"), root: join(dir, "repo") };
  assert.equal(findBinary("kbl-ingest", { ...base, given, envValue: fromEnv }), given);
  assert.equal(findBinary("kbl-ingest", { ...base, envValue: fromEnv }), fromEnv);
  assert.equal(findBinary("kbl-ingest", base), onPath);
});

test("PATH is searched after the build directories", () => {
  const dir = scratch();
  touch(join(dir, "bin", "kbl-mcp"));
  const built = touch(join(dir, "repo", "target/release", "kbl-mcp"));
  assert.equal(findBinary("kbl-mcp", { pathEnv: join(dir, "bin"), home: join(dir, "home"), root: join(dir, "repo") }), built);
  assert.ok(binaryCandidates("kbl-mcp", { pathEnv: "/a:/b", home: "/h", root: "/r" }).includes(resolve("/b/kbl-mcp")));
});

test("the not-found message lists PATH", () => {
  const m = notFoundMessage("kbl-ingest", { flag: "--ingest", envName: "KBL_INGEST", build: "cargo build" });
  assert.match(m, /PATH/);
  assert.match(m, /--ingest/);
});

test("no filter keeps the agent-ok default", () => {
  const r = buildRule({ repo: "o/r" });
  assert.deepEqual(r.labels_all, ["agent-ok"]);
  assert.equal(r.name, "agent-ok");
});

test("a milestone alone drops the agent-ok default and names the milestone", () => {
  const r = buildRule({ repo: "o/r", milestone: "v0.8.0" });
  assert.deepEqual(r.labels_all, []);
  assert.equal(r.milestone, "v0.8.0");
  assert.equal(r.name, "milestone v0.8.0");
});

test("an exclude filter alone drops the default; labels given are kept with the milestone", () => {
  assert.deepEqual(buildRule({ repo: "o/r", excludeLabel: "wontfix" }).labels_all, []);
  const r = buildRule({ repo: "o/r", labels: "bug", milestone: "v1" });
  assert.deepEqual(r.labels_all, ["bug"]);
  assert.equal(r.name, "bug, milestone v1");
});

test("an explicit empty --labels means no label filter, and the repo names the rule", () => {
  const r = buildRule({ repo: "o/r", labels: "" });
  assert.deepEqual(r.labels_all, []);
  assert.equal(r.name, "o/r");
});
