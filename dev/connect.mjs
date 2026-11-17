#!/usr/bin/env node
// Writes an .mcp.json that points an MCP host (Claude Code) at kbl-mcp for the
// quick start stack: the tracker token the stack made for the agent, and an ID
// token in a file of its own that kbl-mcp trades for a Felix token and renews
// from. With --fixed-token it holds a Felix token instead, which kbl-mcp cannot
// renew. Both files are mode 0600, paths are absolute, and nothing is printed
// that contains a token. See "Quick start" in README.md.
//
//   node dev/connect.mjs [agent1] [--out .mcp.json] [--mcp /path/to/kbl-mcp] [--fixed-token]
import { existsSync, readFileSync, writeFileSync, chmodSync, renameSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { parseArgs } from "node:util";
import { pathToFileURL } from "node:url";
import { mcpConfig } from "./agents.mjs";
import { felixToken, idToken } from "./lib/stack.mjs";

const here = dirname(fileURLToPath(import.meta.url));
const repo = resolve(here, "..");

export async function connect(argv) {
  const { values, positionals } = parseArgs({
    args: argv,
    allowPositionals: true,
    options: {
      out: { type: "string", default: ".mcp.json" },
      mcp: { type: "string" },
      run: { type: "string", default: "quickstart" },
      "fixed-token": { type: "boolean", default: false },
    },
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
  let felix;
  if (values["fixed-token"]) {
    felix = { felix: await felixToken() };
  } else {
    // Rewriting this file is enough to hand a running kbl-mcp a new ID token.
    const idTokenFile = join(here, "state", `${name}.id-token`);
    // A temp file renamed over the target: a rerun's mode only applies to a new file.
    const tmpFile = `${idTokenFile}.${process.pid}.tmp`;
    writeFileSync(tmpFile, `${await idToken()}\n`, { mode: 0o600 });
    renameSync(tmpFile, idTokenFile);
    felix = { idTokenFile, controlPlane: process.env.KBL_FELIX_CONTROL_PLANE ?? "http://127.0.0.1:8443" };
  }
  const config = mcpConfig(
    {
      run: values.run,
      caFile: join(here, "state", "broker-cert.pem"),
      trackerUrl: process.env.KBL_TRACKER_URL ?? "http://127.0.0.1:8790",
      mcp,
    },
    { name, tracker, ...felix },
  );
  const out = resolve(values.out);
  writeFileSync(out, `${JSON.stringify(config, null, 2)}\n`, { mode: 0o600 });
  chmodSync(out, 0o600);
  return { out, name, fixed: values["fixed-token"] };
}

if (import.meta.url === pathToFileURL(process.argv[1]).href) {
  connect(process.argv.slice(2)).then(
    ({ out, name, fixed }) =>
      console.log(
        fixed
          ? `wrote ${out} for ${name} (mode 600). Its Felix token lasts 24 hours and is not renewed.`
          : `wrote ${out} for ${name} (mode 600). kbl-mcp renews its Felix token from dev/state/${name}.id-token, which lasts 24 hours.`,
      ),
    (e) => {
      console.error(e.message);
      process.exit(1);
    },
  );
}
