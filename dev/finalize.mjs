// Turns on atomic commits for the fleet. A cluster member serves commits only
// once the fleet has finalized `atomic_commit`, and the control plane accepts
// that only while a broker is serving, so this retries until the broker has
// registered. Uses the broker's own credential, which may manage nodes.
import { readFile, writeFile } from "node:fs/promises";

const CONTROL_PLANE = process.env.AGENTS_FELIX_CONTROL_PLANE ?? "http://controlplane:8443";
const STATE = process.env.AGENTS_STATE_DIR ?? "/state";
const FEATURE = "atomic_commit";

const token = (await readFile(`${STATE}/node.token`, "utf8")).trim();
for (let attempt = 1; attempt <= 60; attempt++) {
  const response = await fetch(`${CONTROL_PLANE}/v1/fleet/features/${FEATURE}/finalize`, {
    method: "POST",
    headers: { authorization: `Bearer ${token}` },
  }).catch((err) => ({ ok: false, status: 0, text: async () => String(err) }));
  if (response.ok) {
    console.log(`${FEATURE} finalized`);
    await writeFile(`${STATE}/finalized`, "");
    process.exit(0);
  }
  console.log(`attempt ${attempt}: ${response.status} ${await response.text()}`);
  await new Promise((resolve) => setTimeout(resolve, 2000));
}
console.error(`${FEATURE} was not finalized`);
process.exit(1);
