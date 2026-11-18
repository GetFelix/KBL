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

console.log(`
KBL is up. Everything listens on 127.0.0.1 only.
  view link (read-only, works once; a reload in the same tab keeps it):
    http://127.0.0.1:8081/?gateway=${gateway}&room=kbl&token=${id_token}
  person token for actions (paste into "Sign in to act"):
    ${me}
  tracker:    http://127.0.0.1:8790 (REST)
Stop it with dev/down.sh. Connect Claude Code with: node dev/connect.mjs`);
