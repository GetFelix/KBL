#!/usr/bin/env node
// Prints where the quick start stack is and the dashboard link, which carries a
// fresh gateway token, and the person token for actions. Both are for this machine only.
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const IDP = process.env.KBL_IDP ?? "http://127.0.0.1:9400";
const { id_token } = await (await fetch(`${IDP}/token?sub=agents-admin&aud=kbl`)).json();
const tokens = JSON.parse(readFileSync(join(here, "state", "tracker-tokens.json"), "utf8"));
const me = Object.entries(tokens.people)[0][0];
const gateway = encodeURIComponent("ws://127.0.0.1:8788/ws");
const TRACKER = process.env.KBL_TRACKER_URL ?? "http://127.0.0.1:8790";

// A new work log starts at the current format. One below it, from an older stack, needs a person to raise it.
async function formatNote() {
  try {
    const reply = await fetch(`${TRACKER}/v1/me`, { headers: { authorization: `Bearer ${me}` } });
    const { teams, current_format } = await reply.json();
    return (teams ?? [])
      .filter((t) => typeof t.log_format === "number")
      .map((t) => {
        const at = `work log format: ${t.log_format} (this build writes ${current_format})`;
        if (t.log_format >= current_format) return `  ${at}`;
        return `  ${at}. Features of the newer formats are refused until a person raises it:
    curl -fsS -X POST ${TRACKER}/v1/work/records -H "authorization: Bearer ${me}" -H 'content-type: application/json' -d '{"record":{"kind":"format_raised","to":${current_format}}}'
    (see docs/upgrades.md#formats)`;
      })
      .join("\n");
  } catch {
    return "";
  }
}
const note = await formatNote();

console.log(`
KBL is up. Everything listens on 127.0.0.1 only.
  view link (read-only, works once; a reload in the same tab keeps it):
    http://127.0.0.1:8081/?gateway=${gateway}&room=kbl&token=${id_token}
  person token for actions (paste into "Sign in to act"):
    ${me}
  tracker:    http://127.0.0.1:8790 (REST)${note ? `\n${note}` : ""}
Stop it with dev/down.sh. Connect Claude Code with: node dev/connect.mjs`);
