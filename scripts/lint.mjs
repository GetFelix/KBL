// Syntax checks that need no dependencies: the style frame's inline module, the
// scripts, and the rule that prose uses no em dashes.
import { execFileSync } from 'node:child_process';
import { mkdtempSync, readFileSync, readdirSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = fileURLToPath(new URL('..', import.meta.url));
const failures = [];
const syntax = (file, label) => {
  try { execFileSync(process.execPath, ['--check', file], { stdio: 'pipe' }); } catch (e) { failures.push(`${label}: ${e.stderr.toString().trim()}`); }
};

const html = readFileSync(join(root, 'prototype/style-frame.html'), 'utf8');
const inline = html.match(/<script type="module">([\s\S]*?)<\/script>/);
if (!inline) failures.push('prototype/style-frame.html: no inline module script');
else {
  const file = join(mkdtempSync(join(tmpdir(), 'fa-lint-')), 'style-frame.mjs');
  writeFileSync(file, inline[1]);
  syntax(file, 'prototype/style-frame.html');
}

for (const dir of ['scripts', 'scripts/lib', 'dev']) {
  for (const f of readdirSync(join(root, dir)).filter((f) => f.endsWith('.mjs'))) syntax(join(root, dir, f), `${dir}/${f}`);
}

for (const f of ['README.md', 'CONTRIBUTING.md', ...readdirSync(join(root, 'docs'), { recursive: true }).filter((f) => f.endsWith('.md')).map((f) => `docs/${f}`)]) {
  if (readFileSync(join(root, f), 'utf8').includes('—')) failures.push(`${f}: contains an em dash`);
}

if (failures.length) { failures.forEach((f) => console.error('FAIL', f)); process.exit(1); }
console.log('lint ok');
