// Fails when a tracker route, an ingest route, an MCP tool or a record kind has no row in
// docs/threat-model.md, or when the doc cites a file and line that is not there.
import { existsSync, readFileSync } from 'node:fs';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = fileURLToPath(new URL('..', import.meta.url));
const read = (f) => readFileSync(join(root, f), 'utf8');
const all = (text, re) => [...text.matchAll(re)].map((m) => m[1]);

const routes = (f) => all(read(f), /\.route\(\s*"([^"]+)"/g);
// The string each arm of `kind()` returns.
const kinds = (f) => all(read(f).split('pub fn kind(')[1], /=> "(\w+)"/g).filter((k) => k !== 'unknown');
const tools = (f) => all(read(f), /#\[tool\([\s\S]*?\)\]\s*(?:async )?fn (\w+)/g);

// What must have a row, under which heading of the doc.
const wanted = {
  'Tracker routes': [...routes('tracker/src/http.rs'), ...routes('tracker/src/http/ingest.rs')],
  'Ingest': routes('ingest/src/webhook.rs'),
  'MCP tools': tools('mcp/src/server.rs'),
  'Records on work.events': kinds('core/src/work.rs'),
  'Records on the run log': kinds('core/src/record.rs'),
};

const doc = read('docs/threat-model.md');
const sections = Object.fromEntries(doc.split(/^## /m).slice(1).map((s) => [s.slice(0, s.indexOf('\n')).trim(), s]));
const failures = [];

for (const [heading, names] of Object.entries(wanted)) {
  const section = sections[heading];
  if (!section) {
    failures.push(`docs/threat-model.md has no "## ${heading}" section`);
    continue;
  }
  if (names.length === 0) failures.push(`found nothing to check for "${heading}"; the source moved, update this script`);
  for (const name of names) {
    if (!section.includes(`\`${name}\``)) failures.push(`"${heading}" has no row for \`${name}\``);
  }
}

for (const [, file, line] of doc.matchAll(/`([\w./-]+\.(?:rs|toml|mjs)):(\d+)`/g)) {
  if (!existsSync(join(root, file))) failures.push(`${file}:${line} is cited but the file does not exist`);
  else if (read(file).split('\n').length < Number(line)) failures.push(`${file}:${line} is cited but the file is shorter`);
}

if (failures.length) {
  console.error(failures.join('\n'));
  console.error('\nAdd the row to docs/threat-model.md: who can call or write it, what an agent controls, and the check.');
  process.exit(1);
}
console.log(`threat model: ${Object.values(wanted).flat().length} routes, tools and record kinds have a row`);
