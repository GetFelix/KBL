import { lstatSync, realpathSync } from "node:fs";
import { basename, dirname, join, resolve, sep } from "node:path";

const inside = (path, root) => path.startsWith(root.endsWith(sep) ? root : root + sep);

// Returns the real path of `path` if it is a regular file (not a symlink)
// inside `root`, and throws otherwise. The worker's path comes from argv, so
// it is checked against the root before any file system call, then again
// after resolving its directory, so a symlinked parent cannot lead out.
export function confinedConfig(path, root) {
  if (!root) throw new Error("FAKE_WORKER_CONFIG_ROOT is not set");
  const givenRoot = resolve(root);
  const realRoot = realpathSync(givenRoot);
  const abs = resolve(path);
  if (!inside(abs, givenRoot) && !inside(abs, realRoot)) {
    throw new Error(`${path} is outside ${root}`);
  }
  const real = join(realpathSync(dirname(abs)), basename(abs));
  if (!inside(real, realRoot)) throw new Error(`${path} is outside ${root}`);
  if (!lstatSync(real).isFile()) throw new Error(`${path} is not a regular file`);
  return real;
}
