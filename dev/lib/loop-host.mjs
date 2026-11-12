// A stand-in for an MCP host such as `claude -p`: it follows the work-loop prompt
// with no model behind it, so a check can run the launcher without one. It reads the
// .mcp.json the launcher wrote (KBL_MCP_CONFIG), starts kbl-mcp from it, and takes
// work_next until nothing is ready.
//
// KBL_STANDIN_DIR and KBL_STANDIN_CRASH_ITEM make the first session that claims the
// named item die holding it, as a killed host would.
import { readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { agent } from "./stack.mjs";

const prompt = process.argv.at(-1);
if (!prompt.includes("work_next") || !prompt.includes("KBL_QUEUE_EMPTY")) {
  console.error("the loop prompt is missing");
  process.exit(3);
}
const { command, env } = Object.values(JSON.parse(readFileSync(process.env.KBL_MCP_CONFIG, "utf8")).mcpServers)[0];
const { KBL_RUN, KBL_AGENT, KBL_FELIX_TOKEN, ...rest } = env;
const session = await agent(KBL_FELIX_TOKEN, KBL_RUN, KBL_AGENT, rest, command);

const dir = process.env.KBL_STANDIN_DIR;
const crashItem = process.env.KBL_STANDIN_CRASH_ITEM;
let failures = 0;
for (;;) {
  const reply = await session.tool("work_next", { wait_seconds: 3 });
  if (!reply.claimed) {
    if (reply.note) {
      await new Promise((r) => setTimeout(r, 1000));
      continue;
    }
    console.log("KBL_QUEUE_EMPTY");
    session.kill();
    process.exit(0);
  }
  const item = reply.item.id;
  console.log(`CLAIM ${item} ${Date.now()}`);
  if (item === crashItem) {
    try {
      writeFileSync(join(dir, "crashed"), JSON.stringify({ agent: KBL_AGENT, item, at: Date.now() }), { flag: "wx" });
      session.kill();
      process.exit(1);
    } catch {
      // Another session crashed first; this one works the item.
    }
  }
  await new Promise((r) => setTimeout(r, 700));
  try {
    await session.tool("work_complete", { item });
  } catch (e) {
    console.error(`could not complete ${item}: ${e.message}`);
    await session.tool("work_release", { item, reason: "stand-in could not complete it" }).catch(() => {});
    if (++failures > 5) process.exit(4);
  }
}
