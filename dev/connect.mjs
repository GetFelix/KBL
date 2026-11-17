#!/usr/bin/env node
// Writes an .mcp.json that points an MCP host (Claude Code) at kbl-mcp for the
// quick start stack: a fresh Felix token for the agent, the tracker token the
// stack made for it, and absolute paths. The file is mode 0600 and nothing is
// printed that contains a token. See "Quick start" in README.md.
//
//   node dev/connect.mjs [agent1] [--out .mcp.json] [--mcp /path/to/kbl-mcp]
import { existsSync, readFileSync, writeFileSync, chmodSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { parseArgs } from "node:util";
import { mcpConfig } from "./agents.mjs";
import { felixToken } from "./lib/stack.mjs";

const here = dirname(fileURLToPath(import.meta.url));
const repo = resolve(here, "..");

export async function connect(argv) {
  const { values, positionals } = parseArgs({
    args: argv,
    allowPositionals: true,
    options: { out: { type: "string", default: ".mcp.json" }, mcp: { type: "string" }, run: { type: "string", default: "quickstart" } },
  });
  const name = positionals[0] ?? "agent1";
  const tokens = JSON.parse(readFileSync(join(here, "state", "tracker-tokens.json"), "utf8"));
  const tracker = Object.entries(tokens.agents).find(([, who]) => who === name)?.[0];
  if (!tracker) {
    throw new Error(`no agent ${name}: the quick start has ${Object.values(tokens.agents).join(", ")}`);
  }
  const candidates = [values.mcp, process.env.KBL_MCP, join(repo, "target/release/kbl-mcp"), join(repo, "target/debug/kbl-mcp")];
  const mcp = candidates.filter(Boolean).map((p) => resolve(p)).find(existsSync);
  if (!mcp) throw new Error("kbl-mcp is not built: run `cargo build --release -p kbl-mcp`, or pass --mcp <path>");
  const config = mcpConfig(
    {
      run: values.run,
      caFile: join(here, "state", "broker-cert.pem"),
      trackerUrl: process.env.KBL_TRACKER_URL ?? "http://127.0.0.1:8790",
      mcp,
    },
    { name, felix: await felixToken(), tracker },
  );
  const out = resolve(values.out);
  writeFileSync(out, `${JSON.stringify(config, null, 2)}\n`, { mode: 0o600 });
  chmodSync(out, 0o600);
  return { out, name };
}

if (import.meta.url === `file://${process.argv[1]}`) {
  connect(process.argv.slice(2)).then(
    ({ out, name }) => console.log(`wrote ${out} for ${name} (mode 600). Its Felix token lasts 24 hours.`),
    (e) => {
      console.error(e.message);
      process.exit(1);
    },
  );
}
