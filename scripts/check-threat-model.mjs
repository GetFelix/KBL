// Fails when a route (per method), an MCP tool or a record kind has no row in
// docs/threat-model.md, or when a `file:line` the doc cites no longer holds the line it was
// pinned to. Routes and tools are found in every workspace crate's src. `--fix` moves a
// citation whose line moved, and pins new citations; check what a new one points at first.
import { existsSync, readFileSync, readdirSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = fileURLToPath(new URL('..', import.meta.url));
const read = (f) => readFileSync(join(root, f), 'utf8');
const all = (text, re) => [...text.matchAll(re)].map((m) => m[1]);
const fix = process.argv.includes('--fix');

const METHODS = ['get', 'post', 'put', 'delete', 'patch', 'head', 'options', 'any'];

// The text from `open` (just after a `(`) to its matching `)`, skipping string literals.
const balanced = (text, open) => {
  let depth = 1;
  for (let i = open; i < text.length; i++) {
    const c = text[i];
    if (c === '"') {
      for (i++; i < text.length && text[i] !== '"'; i++) if (text[i] === '\\') i++;
    } else if (c === '(') depth++;
    else if (c === ')' && --depth === 0) return text.slice(open, i);
  }
  throw new Error('unbalanced parentheses');
};

// Each `.route("path", get(..).post(..))` as {path, methods}. A path that is not a string
// literal, a method this script does not know, or a `.nest(` (whose prefix it cannot add) is an
// error: a route this script cannot read must not pass without a row.
const routesIn = (source, label) => {
  // Comments and inline test modules (cut at the first, which sits at the end by convention)
  // build no served route.
  const text = source.split(/#\[cfg\(test\)\]\s*mod\s+\w+\s*\{/)[0].replace(/^\s*\/\/.*$/gm, '');
  const nest = text.match(/\.(nest|nest_service|route_service)\(/);
  if (nest) throw new Error(`${label}: .${nest[1]}( is not read by this script; update it`);
  const found = [];
  for (const m of text.matchAll(/\.route\(/g)) {
    const args = balanced(text, m.index + m[0].length);
    const lit = args.match(/^\s*"([^"]+)"\s*,([\s\S]*)$/);
    if (!lit) throw new Error(`${label}: a .route( whose path is not a string literal; update this script`);
    // The calls at depth 0 of the method router: `get(..)`, then `.post(..)`.
    const body = lit[2];
    const methods = [];
    let i = 0;
    for (;;) {
      const call = body.slice(i).match(/^[\s.]*([a-z_]\w*)\s*\(/);
      if (!call) break;
      if (!METHODS.includes(call[1])) throw new Error(`${label}: ${lit[1]} uses ${call[1]}(, which this script does not know; update it`);
      methods.push(call[1].toUpperCase());
      const open = i + call[0].length;
      i = open + balanced(body, open).length + 1;
    }
    if (methods.length === 0 || !/^[\s,]*$/.test(body.slice(i))) throw new Error(`${label}: cannot read the methods of ${lit[1]}; update this script`);
    found.push({ path: lit[1], methods });
  }
  return found;
};

// The string each arm of `kind()` returns.
const kinds = (f) => all(read(f).split('pub fn kind(')[1], /=> "(\w+)"/g).filter((k) => k !== 'unknown');
// The function under each `#[tool(...)]`, whatever its visibility. Every attribute must yield a
// name: a shape the pattern does not know would otherwise drop a tool without a word.
const toolNames = (text, label) => {
  const names = all(text, /#\[tool\((?:(?!#\[tool\()[\s\S])*?\)\]\s*(?:pub(?:\([^)]*\))?\s+)?(?:async\s+)?fn\s+(\w+)/g);
  const attrs = text.split('#[tool(').length - 1;
  if (names.length !== attrs) throw new Error(`${label}: ${attrs} #[tool] attributes but ${names.length} tool names; update toolNames in this script`);
  return names;
};

// Unit tests (`tests.rs`, `testing.rs`, anything under `tests/`) build routers of their own.
const isTest = (path) => path.split('/').some((p) => p === 'tests' || p === 'tests.rs' || p === 'testing.rs');

const sources = (dir) =>
  readdirSync(join(root, dir), { withFileTypes: true }).flatMap((e) => {
    const p = `${dir}/${e.name}`;
    if (e.isDirectory()) return sources(p);
    return e.name.endsWith('.rs') && !isTest(p) ? [p] : [];
  });

// The names in the first cell of each table row of a section.
const rowNames = (section) => new Set(section.split('\n').filter((l) => l.startsWith('|')).flatMap((l) => all(l.split('|')[1], /`([^`]+)`/g)));

// A path with one method may be named alone; with more, each `METHOD path` needs a row.
const missingRoutes = (routes, rows) =>
  [...routes].flatMap(([path, methods]) => {
    if (methods.size === 1 && rows.has(path)) return [];
    return [...methods].map((m) => `${m} ${path}`).filter((n) => !rows.has(n));
  });

// Where a pinned line is now: its old line if it still holds the text, else the one line that
// does. A text that appears more than once is taken at `shift` lines from where it was, the
// move of the nearest citation in the file that could be placed. `null` when neither holds.
const relocate = (lines, line, text, shift = 0) => {
  const holds = (n) => (lines[n - 1] ?? '').trim() === text;
  if (holds(line)) return line;
  const hits = lines.flatMap((l, i) => (l.trim() === text ? [i + 1] : []));
  if (hits.length === 1) return hits[0];
  return hits.length > 1 && shift && holds(line + shift) ? line + shift : null;
};

// The shift to try for an ambiguous citation: that of the nearest citation in the same file
// that moved to one place.
const nearestShift = (moves, line) =>
  moves.reduce((best, m) => (!best || Math.abs(m.line - line) < Math.abs(best.line - line) ? m : best), null)?.shift ?? 0;

// A line like `}` or `item,` matches anywhere and says nothing about the check: cite the line
// that does the checking.
// A `fn` line is fine when the whole function is the check.
const telling = (text) => text.length >= 16 && /[a-z]{3}/i.test(text) && !text.startsWith('//');

// Self-test: the shapes the helpers above must handle.
{
  const want = (got, exp, what) => {
    if (JSON.stringify(got) !== JSON.stringify(exp)) throw new Error(`self-test: ${what} were ${JSON.stringify(got)}`);
  };
  const throws = (f, what) => {
    let threw = false;
    try { f(); } catch { threw = true; }
    if (!threw) throw new Error(`self-test: ${what} passed without an error`);
  };
  const sample = '#[tool(description = "a (b)")]\nasync fn one() {}\n#[tool(\n  description = "c"\n)]\npub(crate) async fn two() {}\n#[tool(description = "d")]\npub fn three() {}\n#[tool(description = "e")]\nfn four() {}';
  want(toolNames(sample, 'self-test'), ['one', 'two', 'three', 'four'], 'tool names');
  throws(() => toolNames('#[tool(description = "x")]\nconst fn odd() {}\n#[tool(description = "y")]\nfn ok() {}', 'self-test'), 'an unknown tool shape');
  want([...rowNames('Prose names `prose`.\n| `a`, `b` | mentions `c` |\n|---|---|')], ['a', 'b'], 'row names');
  const router = 'Router::new()\n  .route("/a", post(a::<B>))\n  .route(\n    "/b",\n    get(x::list::<B>).put(x::save::<B>),\n  )\n  .route("/c/{*rest}", any(move |r: Request| f(s.clone(), r)))\n  .route("/h", get(|| async { "o)k" }))\n  .merge(work)';
  want(routesIn(router, 'self-test'), [
    { path: '/a', methods: ['POST'] },
    { path: '/b', methods: ['GET', 'PUT'] },
    { path: '/c/{*rest}', methods: ['ANY'] },
    { path: '/h', methods: ['GET'] },
  ], 'routes');
  throws(() => routesIn('.route(PATH, get(a))', 'self-test'), 'a const path');
  throws(() => routesIn('.route("/a", on(MethodFilter::GET, a))', 'self-test'), 'an unknown method router');
  throws(() => routesIn('.nest("/v2", inner)', 'self-test'), 'a nested router');
  want(routesIn('// not .nest( or .route("/x", get(a))\n.route("/a", get(a))\n#[cfg(test)]\nmod tests {\n  .route("/t", get(t))\n}', 'self-test'), [{ path: '/a', methods: ['GET'] }], 'routes outside comments and tests');
  want(['t/src/a.rs', 't/src/a/tests.rs', 't/src/tests/b.rs', 't/src/testing.rs'].filter((p) => !isTest(p)), ['t/src/a.rs'], 'scanned files');
  const paths = new Map([['/a', new Set(['POST'])], ['/b', new Set(['GET', 'PUT'])]]);
  want(missingRoutes(paths, new Set(['/a', '/b', 'GET /b'])), ['PUT /b'], 'missing route rows');
  want(missingRoutes(paths, new Set(['POST /a', 'GET /b', 'PUT /b'])), [], 'method rows');
  const code = ['fn a() {', '    check();', '}'];
  want([relocate(code, 2, 'check();'), relocate(['', ...code], 2, 'check();'), relocate(code, 2, 'gone();')], [2, 3, null], 'relocations');
  want(relocate(['x();', 'x();'], 3, 'x();'), null, 'an ambiguous relocation');
  want(relocate(['', 'x();', 'y();', 'x();'], 3, 'x();', 1), 4, 'a relocation by its neighbour\'s shift');
  want(nearestShift([{ line: 10, shift: 1 }, { line: 50, shift: 3 }], 40), 3, 'the nearest shift');
  want(['}', 'item,', '// A comment that is long', 'return Err(Reject::NotHeld);'].filter(telling), ['return Err(Reject::NotHeld);'], 'telling lines');
}

// Routes and tools from every workspace crate. The crate decides the section.
const members = all(read('Cargo.toml').match(/members\s*=\s*\[([^\]]*)\]/)[1], /"([^"]+)"/g);
if (members.some((m) => m.includes('*'))) throw new Error('Cargo.toml has a glob member; list members by name or teach this script globs');
const ROUTE_SECTIONS = { tracker: ['Tracker routes', 'Teams'], ingest: ['Ingest'] };
const TOOL_SECTIONS = { mcp: 'MCP tools' };
const failures = [];
const routes = Object.fromEntries(Object.keys(ROUTE_SECTIONS).map((c) => [c, new Map()]));
const toolsFound = Object.fromEntries(Object.keys(TOOL_SECTIONS).map((c) => [c, []]));
for (const crate of members) {
  if (!existsSync(join(root, crate, 'src'))) continue;
  for (const file of sources(`${crate}/src`)) {
    const text = read(file);
    const found = routesIn(text, file);
    if (found.length && !routes[crate]) failures.push(`${file} serves routes, but this script has no threat-model section for the ${crate} crate`);
    for (const { path, methods } of found) {
      const seen = routes[crate]?.get(path) ?? new Set();
      methods.forEach((m) => seen.add(m));
      routes[crate]?.set(path, seen);
    }
    const tools = text.includes('#[tool(') ? toolNames(text, file) : [];
    if (tools.length && !toolsFound[crate]) failures.push(`${file} defines MCP tools, but this script has no threat-model section for the ${crate} crate`);
    toolsFound[crate]?.push(...tools);
  }
}

const doc = read('docs/threat-model.md');
const sections = Object.fromEntries(doc.split(/^## /m).slice(1).map((s) => [s.slice(0, s.indexOf('\n')).trim(), s]));
const section = (heading) => {
  if (!sections[heading]) failures.push(`docs/threat-model.md has no "## ${heading}" section`);
  return sections[heading] ?? '';
};

let count = 0;
for (const [crate, headings] of Object.entries(ROUTE_SECTIONS)) {
  const found = routes[crate];
  if (found.size === 0) failures.push(`found no routes in ${crate}/src; the source moved, update this script`);
  const rows = new Set(headings.flatMap((h) => [...rowNames(section(h))]));
  for (const name of missingRoutes(found, rows)) failures.push(`"${headings.join('" or "')}" has no row for \`${name}\``);
  count += [...found.values()].reduce((n, m) => n + m.size, 0);
}

const named = {
  ...Object.fromEntries(Object.entries(TOOL_SECTIONS).map(([crate, h]) => [h, toolsFound[crate]])),
  'Records on work.events': kinds('core/src/work.rs'),
  'Records on org.events': kinds('core/src/org.rs'),
  'Records on the run log': kinds('core/src/record.rs'),
};
for (const [heading, names] of Object.entries(named)) {
  if (names.length === 0) failures.push(`found nothing to check for "${heading}"; the source moved, update this script`);
  const rows = rowNames(section(heading));
  for (const name of names) if (!rows.has(name)) failures.push(`"${heading}" has no row for \`${name}\``);
  count += names.length;
}

// Each cited line is pinned to its text in a file beside the doc, so a citation that drifts
// fails here instead of pointing at the wrong code.
const PINS = 'docs/threat-model-citations.json';
const pins = existsSync(join(root, PINS)) ? JSON.parse(read(PINS)) : {};
const used = {};
const cites = [...new Set([...doc.matchAll(/`[\w./-]+\.(?:rs|toml|mjs):\d+`/g)].map((m) => m[0]))];
const moved = {};
const placed = [];
for (const cite of cites) {
  const [, file, line] = cite.match(/`(.+):(\d+)`/);
  const key = `${file}:${line}`;
  if (!existsSync(join(root, file))) {
    failures.push(`${key} is cited but the file does not exist`);
    continue;
  }
  const text = pins[key];
  if (text === undefined) {
    const now = (read(file).split('\n')[Number(line) - 1] ?? '').trim();
    if (!telling(now)) failures.push(`${key} is cited but that line (\`${now}\`) is not one that checks anything; cite the line that does`);
    else if (fix) used[key] = now;
    else failures.push(`${key} is a new citation; check it points at the right line, then pin it with node scripts/check-threat-model.mjs --fix`);
    continue;
  }
  if (!telling(text)) failures.push(`${key} is pinned to \`${text}\`, which is not a line that checks anything; cite the line that does`);
  placed.push({ cite, file, line: Number(line), text });
}
// Unique texts first, so an ambiguous one can follow its neighbours' shift.
const moves = {};
for (const pass of [false, true]) {
  for (const c of placed) {
    if (c.done) continue;
    const lines = read(c.file).split('\n');
    const at = relocate(lines, c.line, c.text, pass ? nearestShift(moves[c.file] ?? [], c.line) : 0);
    if (!at && !pass) continue;
    c.done = true;
    if (at === c.line) used[`${c.file}:${c.line}`] = c.text;
    else if (at && fix) {
      moved[c.cite] = `\`${c.file}:${at}\``;
      used[`${c.file}:${at}`] = c.text;
    } else if (at) failures.push(`${c.file}:${c.line} moved to line ${at}; run node scripts/check-threat-model.mjs --fix`);
    else {
      // Kept, so a later --fix cannot pin whatever line lands at this number.
      used[`${c.file}:${c.line}`] = c.text;
      failures.push(`${c.file}:${c.line} no longer holds \`${c.text}\`, and no single line of ${c.file} does; cite what the row means now`);
    }
    if (at && !pass) (moves[c.file] ??= []).push({ line: c.line, shift: at - c.line });
  }
}
// One pass over the doc, so a citation moved onto another's old line is not moved twice.
const fixed = doc.replace(/`[\w./-]+\.(?:rs|toml|mjs):\d+`/g, (c) => moved[c] ?? c);
if (!fix) for (const key of Object.keys(pins)) if (!cites.includes(`\`${key}\``)) failures.push(`${PINS} pins ${key}, which the doc no longer cites; run --fix`);
if (fix) {
  writeFileSync(join(root, 'docs/threat-model.md'), fixed);
  const sorted = Object.fromEntries(Object.entries(used).sort(([a], [b]) => a.localeCompare(b, 'en', { numeric: true })));
  writeFileSync(join(root, PINS), `${JSON.stringify(sorted, null, 1)}\n`);
}

if (failures.length) {
  console.error(failures.join('\n'));
  console.error('\nAdd the row to docs/threat-model.md: who can call or write it, what an agent controls, and the check.');
  process.exit(1);
}
console.log(`threat model: ${count} routes, tools and record kinds have a row; ${cites.length} citations hold`);
