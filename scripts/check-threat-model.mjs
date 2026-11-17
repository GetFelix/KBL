// Fails when a route (per method), an MCP tool or a record kind has no row in
// docs/threat-model.md, or when a `file#symbol` the doc cites no longer resolves to exactly one
// function, constant or type that still holds the lines pinned to it in
// docs/threat-model-citations.json. Routes and tools are found in every workspace crate's src.
// Citations name a symbol, not a line, so code that moves around them breaks nothing.
import { existsSync, readFileSync, readdirSync } from 'node:fs';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = fileURLToPath(new URL('..', import.meta.url));
const read = (f) => readFileSync(join(root, f), 'utf8');
const all = (text, re) => [...text.matchAll(re)].map((m) => m[1]);

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

const DECL = /^(\s*)(?:pub(?:\([^)]*\))?\s+)?(?:async\s+)?(?:unsafe\s+)?(?:const\s+)?(?:fn|const|static|struct|enum|trait|type)\s+(\w+)/;
const indent = (l) => l.match(/^\s*/)[0].length;

// The lines of the one item a symbol names, or an error string. `name` is `fn_or_const` or
// `Type::method`; a name declared more than once in the file must be written with its type.
// An item runs to the first later line that closes at its own indent, or to its `;`.
const resolve = (lines, symbol) => {
  const parts = symbol.split('::');
  if (parts.length > 2) return { error: `${symbol} has more than one \`::\`; name the type and the item only` };
  const name = parts.pop();
  let from = 0;
  let to = lines.length;
  if (parts.length) {
    const heads = lines.flatMap((l, i) => (/^\s*impl\b/.test(l) && new RegExp(`\\b${parts[0]}\\b`).test(l.replace(/^\s*impl(<[^>]*>)?/, '').split(/\bfor\b/).pop() ?? '') ? [i] : []));
    if (heads.length !== 1) return { error: `${symbol}: found ${heads.length} impl blocks for ${parts[0]}, need exactly one` };
    from = heads[0] + 1;
    to = lines.findIndex((l, i) => i > heads[0] && indent(l) === indent(lines[heads[0]]) && l.trimStart().startsWith('}'));
    if (to < 0) return { error: `${symbol}: cannot find the end of the impl block` };
  }
  const hits = [];
  for (let i = from; i < to; i++) {
    const m = lines[i].match(DECL);
    if (m && m[2] === name) hits.push(i);
  }
  if (hits.length === 0) return { error: `${symbol} is not declared in the file` };
  if (hits.length > 1) return { error: `${symbol} is declared ${hits.length} times in the file; write it as Type::${name}` };
  const start = hits[0];
  // A constant or alias ends at its `;`; anything with a body, at the `}` on its own indent.
  let end = start;
  let opened = false;
  for (; end < lines.length - 1; end++) {
    const l = lines[end];
    opened ||= l.includes('{');
    if (!opened && /;\s*$/.test(l)) break;
    if (opened && end > start && indent(l) === indent(lines[start]) && l.trimStart().startsWith('}')) break;
    if (end === start && /\{\s*\}\s*;?\s*$/.test(l)) break;
  }
  return { body: lines.slice(start, end + 1).map((l) => l.trim()) };
};

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
  const code = ['impl Core {', '    fn a() {', '        check();', '    }', '    pub(crate) fn b(', '        x: u8,', '    ) -> u8 {', '        other();', '    }', '}', 'impl Other {', '    fn a() {}', '}', 'const LIMIT: usize = 4;'];
  want(resolve(code, 'Core::a').body, ['fn a() {', 'check();', '}'], 'a method by type');
  want(resolve(code, 'b').body.includes('other();'), true, 'a multi-line signature');
  want(resolve(code, 'LIMIT').body, ['const LIMIT: usize = 4;'], 'a constant');
  want(['a', 'gone', 'Other::a', 'Core::gone'].map((n) => Boolean(resolve(code, n).error)), [true, true, false, true], 'ambiguous and missing symbols');
  want(['}', 'item,', '// A comment that is long', 'return Err(Reject::NotHeld);'].filter(telling), ['return Err(Reject::NotHeld);'], 'telling lines');
}

// Routes and tools from every workspace crate. The crate decides the section.
const members = all(read('Cargo.toml').match(/members\s*=\s*\[([^\]]*)\]/)[1], /"([^"]+)"/g);
if (members.some((m) => m.includes('*'))) throw new Error('Cargo.toml has a glob member; list members by name or teach this script globs');
const ROUTE_SECTIONS = { tracker: ['Tracker routes', 'Teams'], ingest: ['Ingest'], metrics: ['Metrics endpoint'] };
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

// Each citation names a symbol; the lines pinned to it must still be in its body, so a rename,
// a deletion or a check that moved out of the function fails here.
const PINS = 'docs/threat-model-citations.json';
const pins = existsSync(join(root, PINS)) ? JSON.parse(read(PINS)) : {};
const cites = [...new Set([...doc.matchAll(/`([\w./-]+\.(?:rs|toml|mjs))#(\w+(?:::\w+)?)`/g)].map((m) => m[0]))];
for (const stale of doc.matchAll(/`[\w./-]+\.(?:rs|toml|mjs):\d+`/g)) failures.push(`${stale[0]} cites a line; cite \`file#symbol\` instead`);
for (const cite of cites) {
  const [, file, symbol] = cite.match(/`(.+)#(.+)`/);
  const key = `${file}#${symbol}`;
  if (!existsSync(join(root, file))) {
    failures.push(`${key} is cited but the file does not exist`);
    continue;
  }
  const found = resolve(read(file).split('\n'), symbol);
  if (found.error) {
    failures.push(`${key}: ${found.error}`);
    continue;
  }
  const texts = pins[key];
  if (!Array.isArray(texts) || texts.length === 0) {
    failures.push(`${key} is a new citation; add "${key}": ["<exact line it relies on>"] to ${PINS}`);
    continue;
  }
  for (const text of texts) {
    if (!telling(text)) failures.push(`${key} is pinned to \`${text}\`, which is not a line that checks anything; pin the line that does`);
    else if (!found.body.includes(text)) failures.push(`${key} no longer holds \`${text}\`; cite what the row means now`);
  }
}
for (const key of Object.keys(pins)) if (!cites.includes(`\`${key}\``)) failures.push(`${PINS} pins ${key}, which the doc no longer cites; remove it`);

if (failures.length) {
  console.error(failures.join('\n'));
  console.error('\nAdd the row to docs/threat-model.md: who can call or write it, what an agent controls, and the check.');
  process.exit(1);
}
console.log(`threat model: ${count} routes, tools and record kinds have a row; ${cites.length} citations hold`);
