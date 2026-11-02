// Turns on the features a new cluster leaves to the operator. A cluster member
// serves atomic commits only once the fleet has finalized `atomic_commit`, and
// stores who published each record only once it has finalized
// `publisher_principal`, which the run log's attribution depends on. The
// control plane accepts either only while a broker is serving, so this retries
// until the broker has registered. Uses the broker's own credential, which may
// manage nodes.
import { readFile, writeFile } from "node:fs/promises";

const CONTROL_PLANE = process.env.AGENTS_FELIX_CONTROL_PLANE ?? "http://controlplane:8443";
const STATE = process.env.AGENTS_STATE_DIR ?? "/state";
const FEATURES = ["atomic_commit", "publisher_principal"];

const token = (await readFile(`${STATE}/node.token`, "utf8")).trim();
for (const feature of FEATURES) {
  let done = false;
  for (let attempt = 1; attempt <= 60 && !done; attempt++) {
    const response = await fetch(`${CONTROL_PLANE}/v1/fleet/features/${feature}/finalize`, {
      method: "POST",
      headers: { authorization: `Bearer ${token}` },
    }).catch((err) => ({ ok: false, status: 0, text: async () => String(err) }));
    if (response.ok) {
      console.log(`${feature} finalized`);
      done = true;
    } else {
      console.log(`${feature}, attempt ${attempt}: ${response.status} ${await response.text()}`);
      await new Promise((resolve) => setTimeout(resolve, 2000));
    }
  }
  if (!done) {
    console.error(`${feature} was not finalized`);
    process.exit(1);
  }
}
await writeFile(`${STATE}/finalized`, "");
