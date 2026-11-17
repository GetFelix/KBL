#!/usr/bin/env node
// Prints where the quick start stack is and the dashboard link, which carries a
// fresh sign-in token. The link and the "me" token are for this machine only.
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
  dashboard:  http://127.0.0.1:8081/?gateway=${gateway}&room=kbl&token=${id_token}
  tracker:    http://127.0.0.1:8790 (REST)
  sign in to the dashboard's writes with this token: ${me}
Stop it with dev/down.sh. Connect Claude Code with: node dev/connect.mjs`);
