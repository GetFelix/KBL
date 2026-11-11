// Builds ../wasm twice: for the browser (Vite imports it) and for node (the tests load it).
// Needs the wasm32-unknown-unknown target and a wasm-bindgen CLI at the version in Cargo.lock.
import { execFileSync } from 'node:child_process';
import { readFileSync, writeFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';

const dashboard = fileURLToPath(new URL('..', import.meta.url));
const repo = fileURLToPath(new URL('../..', import.meta.url));
const target = process.env.CARGO_TARGET_DIR ?? `${repo}/target`;
const run = (cmd, args) => execFileSync(cmd, args, { cwd: repo, stdio: 'inherit' });
const bindgen = process.env.WASM_BINDGEN ?? 'wasm-bindgen';

// The CLI and the crate must match exactly, or the generated glue fails at load.
const locked = readFileSync(`${repo}/Cargo.lock`, 'utf8').match(/name = "wasm-bindgen"\nversion = "([^"]+)"/)?.[1];
const have = execFileSync(bindgen, ['--version'], { encoding: 'utf8' }).trim().split(' ')[1];
if (locked !== have) {
  console.error(`wasm-bindgen CLI is ${have}, Cargo.lock has ${locked}: cargo install wasm-bindgen-cli --version ${locked} --locked`);
  process.exit(1);
}

run('cargo', ['build', '-p', 'agents-wasm', '--lib', '--release', '--target', 'wasm32-unknown-unknown', '--locked']);
const wasm = `${target}/wasm32-unknown-unknown/release/agents_wasm.wasm`;
run(bindgen, ['--target', 'web', '--out-dir', `${dashboard}/src/wasm`, wasm]);
run(bindgen, ['--target', 'nodejs', '--out-dir', `${dashboard}/.wasm-node`, wasm]);
// This package is ESM; the nodejs build is CommonJS.
writeFileSync(`${dashboard}/.wasm-node/package.json`, '{"type":"commonjs"}\n');
