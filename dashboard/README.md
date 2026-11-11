# Dashboard

The Kibble dashboard in the browser. It reads `work.events` through
[felix-gateway](https://github.com/GetFelix/felix-gateway), folds it with the `core` crate compiled to
WebAssembly, and lists every item with its stage. The page is plain TypeScript built with Vite. The fold already
holds the state, so there is no UI framework and no second state layer.

This is the data path only. The page is a list; the real views come from the UX work in
[#132](https://github.com/GetFelix/felix-agents/issues/132). It cannot write: a person's actions will go to the
tracker's REST routes.

## Run it

You need Node 22.18 or later, Rust with the `wasm32-unknown-unknown` target, and a `wasm-bindgen` CLI at the
version in `Cargo.lock` (`npm run wasm` says which if it differs).

```bash
rustup target add wasm32-unknown-unknown
cargo install wasm-bindgen-cli --version <version in Cargo.lock> --locked
cd dashboard
npm ci
npm run dev     # builds the WebAssembly, then starts Vite
npm test        # the fold against the golden logs, and the read-then-follow join
npm run build   # typecheck and production build into dist/
```

The gateway needs a scope that gives the page `subscribe` on `work.events`. `read` is a gateway feature the page
asks for in `join`, not a scope action: a `read` of a stream needs `subscribe` on it, so one grant covers both.

```toml
[scope]
field = "room"

[[scope.streams]]
alias = "events"
name = "work.events"
actions = ["subscribe"]
```

Open the page with the gateway, the scope's value and an ID token in the query string:

```
http://localhost:5173/?gateway=ws://127.0.0.1:8787/ws&room=kibble&token=<ID token>
```

`stream=<alias>` names a different alias than `events`.

The token is read once and then removed from the address with `history.replaceState`. It stays in memory only,
so reloading the page needs the link again. Two cautions: use `wss://` for any gateway but your own machine,
since `ws://` sends the token in the clear; and treat a `?gateway=` link from someone else as untrusted, because
the page sends the token to whatever address it names.

An expired or rejected token (`signed_out`, `forbidden`) stops the page with a message. Other failures retry
from the last applied offset with a growing delay, up to 30 s.

## How it follows the log

```mermaid
sequenceDiagram
    participant P as Page
    participant G as Gateway
    P->>G: read from 0
    G-->>P: records, next_offset
    P->>G: read from next_offset, until a page is empty
    P->>G: subscribe from next_offset
    G-->>P: subscribed, start_offset = next_offset
    G-->>P: events from next_offset on
```

A publish that lands after the last read page is already in the log when the subscribe registers, so the
subscribe delivers it as history. Nothing is missed, and nothing is read twice, because the subscribe starts
exactly where the read stopped. The page also drops any offset it has already folded, and subscribes again from
its cursor if an event arrives after a gap that `skipped_before` does not explain. The gateway has no
unsubscribe: a second `subscribe` on the stream replaces the first, and the page ignores whatever arrives
before the new `subscribed`, since that belongs to the replaced one. Records the fold does not read are counted
in the status line.

## Files

- `src/follow.ts`: the read-then-subscribe join, written against a small `Transport` so tests can fake the gateway.
- `src/gateway.ts`: the WebSocket transport and the `join` handshake.
- `src/fold.ts`: the shape of the WebAssembly fold.
- `../wasm`: the Rust crate that wraps `core`'s fold for the page.
- `../wasm/golden`: logs and the state the native fold gives for each. The wasm fold must match them in
  `test/fold.test.ts`, and the native one in `wasm/tests/golden.rs`.
