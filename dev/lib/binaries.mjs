// Where the dev scripts look for a KBL binary (kbl-mcp, kbl-ingest). An explicit
// path or the environment variable wins; then a source build, ~/.kbl/bin and
// dev/state/bin; PATH comes last so a release installed with `install` is found
// without stealing from a fresh build.
import { existsSync } from "node:fs";
import { homedir } from "node:os";
import { delimiter, dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const dev = resolve(here, "..");
const repo = resolve(dev, "..");

/** The places to look for `name`, in order. Entries may be undefined (an unset flag). */
export function binaryCandidates(name, { given, envValue, pathEnv = process.env.PATH, home = homedir(), root = repo } = {}) {
  const onPath = (pathEnv ?? "").split(delimiter).filter(Boolean).map((d) => join(d, name));
  return [
    given,
    envValue,
    join(root, "target/release", name),
    join(root, "target/debug", name),
    join(home, ".kbl/bin", name),
    join(root, "dev/state/bin", name),
    ...onPath,
  ]
    .filter(Boolean)
    .map((p) => resolve(p));
}

/** The first existing candidate, or undefined. */
export function findBinary(name, options) {
  return binaryCandidates(name, options).find(existsSync);
}

/** The "not found" message, naming the flag, the variable and every place searched. */
export function notFoundMessage(name, { flag, envName, build }) {
  return `${name} not found: pass ${flag} <path>, set ${envName}, put it on your PATH, in ~/.kbl/bin or in dev/state/bin, or run \`${build}\``;
}
