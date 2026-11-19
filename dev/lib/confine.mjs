import { lstatSync, realpathSync } from "node:fs";
import { basename, join, relative, resolve, sep } from "node:path";

// Returns the real path of `path` if it is a regular file (not a symlink)
// inside `root`, and throws otherwise. The worker's path comes from argv, so
// it is only trusted once it is pinned under the supervisor's state dir.
export function confinedConfig(path, root) {
  if (!root) throw new Error("FAKE_WORKER_CONFIG_ROOT is not set");
  const realRoot = realpathSync(resolve(root));
  const abs = resolve(path);
  // Resolve the directory, so a symlinked parent cannot lead out of the root.
  const real = join(realpathSync(resolve(abs, "..")), basename(abs));
  const rel = relative(realRoot, real);
  if (rel === "" || rel === ".." || rel.startsWith(`..${sep}`) || resolve(rel) === rel) {
    throw new Error(`${path} is outside ${root}`);
  }
  if (!lstatSync(real).isFile()) throw new Error(`${path} is not a regular file`);
  return real;
}
