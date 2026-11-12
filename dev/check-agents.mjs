// Runs the launcher (dev/agents.mjs) with a scripted stand-in for the host, so no
// model is needed:
//   1. three sessions, each with its own tracker token, empty a queue of items;
//   2. a session that dies holding an item loses it to another within the claim
//      timeout, and the launcher starts that session again;
//   3. the launcher ends by itself once every session has found the queue empty.
//
//   cargo build -p kbl-mcp -p kbl-tool -p kbl-tracker && node dev/check-agents.mjs
import { spawn } from "node:child_process";
import { mkdtempSync, readFileSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import {
  check,
  createRun,
  felixToken,
  giveUpAfter,
  killAll,
  newRunId,
  personRecord,
  startTracker,
  TRACKER_URL,
  workLog,
} from "./lib/stack.mjs";

const RUN = newRunId("agents");
const NAMES = ["w1", "w2", "w3"];
const ITEMS = Array.from({ length: 15 }, (_, n) => `loop-${n + 1}`);
const CRASH = "loop-4";
const CLAIM_TIMEOUT_MS = 8000;
// The crashed session is started again only after the others had time to take its item.
const RESTART_DELAY_S = 20;

giveUpAfter(200, "the agents check");

const felix = await felixToken();
createRun(RUN, "agents check");
const dir = mkdtempSync(join(tmpdir(), "agents-check-"));
const tokensFile = join(dir, "tokens.json");
writeFileSync(
  tokensFile,
  JSON.stringify(Object.fromEntries(NAMES.map((n) => [n, { felix, tracker: `tok-${n}` }]))),
  { mode: 0o600 },
);

const tracker = await startTracker({
  agents: Object.fromEntries(NAMES.map((n) => [`tok-${n}`, n])),
  people: { "tok-gab": "gab" },
});
let launcher;
try {
  for (const id of ITEMS) {
    await personRecord("tok-gab", { kind: "imported", item: id, source: "check", upstream_id: id, title: `item ${id}` });
    await personRecord("tok-gab", { kind: "accepted", item: id, tier: 1 });
  }

  launcher = spawn(
    "node",
    [
      "dev/agents.mjs",
      "--run", RUN,
      "--tokens", tokensFile,
      "--ca-file", resolve(process.env.KBL_FELIX_CA_FILE ?? "dev/state/broker-cert.pem"),
      "--tracker-url", TRACKER_URL,
      "--mcp", resolve(process.env.KBL_MCP ?? "target/debug/kbl-mcp"),
      "--host", "node",
      "--host-arg", resolve("dev/lib/loop-host.mjs"),
      "--restart-delay", String(RESTART_DELAY_S),
      "--idle-delay", "3",
    ],
    {
      env: {
        ...process.env,
        KBL_CLAIM_VISIBILITY_MS: String(CLAIM_TIMEOUT_MS),
        KBL_EXTEND_EVERY_MS: "1000",
        KBL_STANDIN_DIR: dir,
        KBL_STANDIN_CRASH_ITEM: CRASH,
      },
      stdio: ["ignore", "pipe", "inherit"],
    },
  );
  let output = "";
  launcher.stdout.on("data", (chunk) => {
    output += chunk;
    process.stdout.write(chunk);
  });
  const code = await new Promise((done) => launcher.on("exit", done));
  check(code === 0, "the launcher ended by itself once every session found the queue empty");

  const log = workLog().filter((r) => ITEMS.includes(r.item));
  for (const id of ITEMS) {
    const done = log.filter((r) => r.kind === "done" && r.item === id);
    check(done.length === 1, `${id} was finished once`);
  }
  const claimants = new Set(log.filter((r) => r.kind === "claimed").map((r) => r.by));
  check(NAMES.every((n) => claimants.has(n)), "each of the three sessions took work under its own name");

  const crashed = JSON.parse(readFileSync(join(dir, "crashed"), "utf8"));
  check(crashed.item === CRASH, `a session died holding ${CRASH}`);
  const doneBy = log.find((r) => r.kind === "done" && r.item === CRASH).by;
  check(doneBy !== crashed.agent, `${CRASH} went to ${doneBy}, not back to ${crashed.agent}`);
  const claims = [...output.matchAll(/^\[(\w+)\] CLAIM (\S+) (\d+)$/gm)]
    .filter((m) => m[2] === CRASH)
    .map((m) => ({ by: m[1], at: Number(m[3]) }));
  const takeover = claims.find((c) => c.by !== crashed.agent);
  check(takeover && takeover.at - crashed.at < CLAIM_TIMEOUT_MS + 10_000, "another session claimed it within the claim timeout, plus the sweep");
  check(output.includes(`restarting ${crashed.agent}`), `the launcher started ${crashed.agent} again`);
} finally {
  launcher?.kill("SIGKILL");
  tracker.kill();
  killAll();
}
