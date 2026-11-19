// node --test dev/confine.test.mjs
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { chmodSync, mkdirSync, mkdtempSync, symlinkSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import test from "node:test";

import { confinedConfig } from "./lib/confine.mjs";

const base = mkdtempSync(join(tmpdir(), "confine-"));
const root = join(base, "state");
mkdirSync(join(root, "w"), { recursive: true });
const inside = join(root, "w", "mcp.json");
writeFileSync(inside, "{}", { mode: 0o600 });
const outside = join(base, "other.json");
writeFileSync(outside, "{}", { mode: 0o600 });
chmodSync(outside, 0o600);
symlinkSync(outside, join(root, "w", "link.json"));
symlinkSync(base, join(root, "up"));

const worker = (path) =>
  spawnSync(process.execPath, [resolve("dev/fake-worker.mjs"), path], {
    env: { PATH: process.env.PATH, FAKE_WORKER_CONFIG_ROOT: root },
    encoding: "utf8",
  });

test("a file under the root is accepted", () => {
  assert.ok(confinedConfig(inside, root).endsWith("mcp.json"));
});
test("a path outside the root is refused", () => {
  assert.throws(() => confinedConfig(outside, root), /outside/);
  assert.throws(() => confinedConfig(join(root, "..", "other.json"), root), /outside/);
});
test("a symlink is refused", () => {
  assert.throws(() => confinedConfig(join(root, "w", "link.json"), root), /not a regular file/);
  assert.throws(() => confinedConfig(join(root, "up", "other.json"), root), /outside/);
});
test("fake-worker exits 4 for both", () => {
  for (const p of [outside, join(root, "w", "link.json")]) {
    const r = worker(p);
    assert.equal(r.status, 4);
    assert.match(r.stderr, /refusing the config/);
  }
});
test("the redo copy regex matches what it did", () => {
  for (const [s, want] of [["3 left now", true], ["12 left now", true], ["x left now", false], ["left now", false]]) {
    assert.equal(/\d left now/.test(s), want);
  }
});
