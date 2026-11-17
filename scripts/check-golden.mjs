// A pull request that changes a stored fold result in wasm/golden must say so in a changelog
// fragment of its own. Usage: node scripts/check-golden.mjs [base-ref]  (default origin/main).
// Every changed golden file (added, edited or removed) must have its name, such as
// `format_3`, in a fragment the pull request adds under changelog.d/.
import { execFileSync } from 'node:child_process';
import { readFileSync } from 'node:fs';
import { basename } from 'node:path';

const base = process.argv[2] ?? 'origin/main';
const git = (...a) => execFileSync('git', a, { encoding: 'utf8' }).trim().split('\n').filter(Boolean);
const changed = git('diff', '--name-status', '--no-renames', `${base}...HEAD`);
const files = (dir, ext) => changed
  .map((l) => l.split('\t'))
  .filter(([, p]) => p.startsWith(dir) && p.endsWith(ext));

const goldens = files('wasm/golden/', '.json').concat(files('wasm/golden/', '.jsonl'));
const stems = [...new Set(goldens.map(([, p]) => basename(p).replace(/\.(state\.json|jsonl)$/, '')))];
if (!stems.length) { console.log('golden gate: no golden file changed'); process.exit(0); }

const fragments = files('changelog.d/', '.md')
  .filter(([s, p]) => s !== 'D' && basename(p) !== 'README.md' && git('ls-files', p).length)
  .map(([, p]) => readFileSync(p, 'utf8'));
const missing = stems.filter((s) => !fragments.some((f) => f.includes(s)));
if (missing.length) {
  console.error(`FAIL: wasm/golden changed for ${missing.join(', ')} with no changelog.d fragment naming it.`);
  console.error('Add changelog.d/<name>.<section>.md that says what changed in the fold result (docs/upgrades.md, "Changing a golden on purpose").');
  process.exit(1);
}
console.log(`golden gate ok: ${stems.join(', ')}`);
