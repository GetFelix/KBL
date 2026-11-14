# Dashboard

The KBL (Kanban By Log) dashboard in the browser. It reads `work.events` through
[felix-gateway](https://github.com/GetFelix/felix-gateway), folds it with the `core` crate compiled to
WebAssembly, and lists every item grouped by stage, with a detail panel, search and a command palette. The page
is plain TypeScript built with Vite. The fold already holds the state, so there is no UI framework; a small store
in `src/state.ts` holds only what the view adds (selection, search, open dialogs).

It cannot write yet: a person's actions will go to the tracker's REST routes. The design, the conventions it
follows and the plan for later views are in [docs/design/dashboard-ux.md](../docs/design/dashboard-ux.md).

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
npm run check   # after build: keyboard flows and axe-core in headless Chrome (set CHROME if it is not found)
```

`npm run check` drives `dist/` against a fake gateway (`check/fixture.mjs`) and fails on a broken flow or a
serious or critical axe-core violation. `node check/browser.mjs <dir>` also writes screenshots to `<dir>`.

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
http://localhost:5173/?gateway=ws://127.0.0.1:8787/ws&room=kbl&token=<ID token>
```

`stream=<alias>` names a different alias than `events`.

The token is read once and then removed from the address with `history.replaceState`. It stays in memory only,
so reloading the page needs the link again. The first GET still carries it in the URL, so it reaches the dev
server's access log. Two cautions: use `wss://` for any gateway but your own machine,
since `ws://` sends the token in the clear; and treat a `?gateway=` link from someone else as untrusted, because
the page sends the token to whatever address it names.

An expired or rejected token (`signed_out`, `forbidden`) or an error a retry cannot fix (`trimmed`, `bad_request`,
`past_end`) stops the page with a message, and for a token it adds that a new sign-in link fixes it. A record of
a newer format than the page's fold reads stops it too: the page asks for a reload instead of skipping it ([docs/upgrades.md](../docs/upgrades.md)). `subscription_ended` (a broker failover) subscribes again at once from
the cursor. Any other failure reconnects from the cursor, which is past records the fold refused, with a growing
delay up to 30 s.

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

## Keys

| Key | Does |
| --- | --- |
| `Ctrl+K` (`Cmd+K` on a Mac) | Command palette: every command, plus items by id or title |
| `?` | Keyboard shortcuts |
| `/` | Search |
| `J` / `K`, `↓` / `↑` | Next and previous item (on the board, within the column) |
| `H` / `L`, `←` / `→` | Previous and next column on the board |
| `V` | Switch list and board |
| `F` | Go to the filters |
| `Home` / `End` | First and last item; on the board, of the current column |
| `Enter` | Open the selected item in the panel |
| `Esc` | Close the panel or dialog, or clear the search |

Single-letter keys do nothing while you type in a field. `src/keys.ts` is the one list: the key handler, the
palette and the `?` overlay all read it. Theme and density are in the palette
only. Clear filters and Save view are buttons above the work as well as palette commands, and the palette also
has one entry per filter value and per saved view.

## Views

- **Board.** One column per stage, in the list's order. Each column is one `Tab` stop; the arrows or `H` `J` `K`
  `L` move inside the board. Below 720 px the columns stack. When a record moves the focused card to another
  column the focus goes with it; when one takes it out of the view, the next card in its column (else the nearest
  card, else the work list) takes the focus. Either is announced. The list does the same with its rows.
- **Filters.** Source, tier and repo, each offering only values the log recorded. A value's count is what picking
  it would show under the other filters. A facet no item recorded is not shown. The bar says how many items the
  search and filters leave, with Clear filters. A select is updated in place and left alone while it has focus.
- **The address.** `view=board`, `q=`, `source=`, `tier=`, `repo=` and `item=` are in the address, so a link
  opens the same view.
- **Saved views.** Save view names the current layout, search and filters and puts it in the sidebar; Edit
  renames or deletes it, and a name another view has is refused. After a rename the focus is back on that view's
  Edit; after a delete, on the next saved view or Save view. Saved views and the density are kept in this
  browser's local storage, per viewer, and the page works without them when storage is blocked. Each change
  re-reads storage first and the page follows changes from other tabs, so two tabs do not drop each other's views.
- **Phones.** Below 720 px the sidebar is a drawer behind the Menu button. While it is open the page behind it
  is inert under a backdrop and `Tab` stays inside it; `Esc`, the backdrop or a choice closes it.

## Files

- `src/follow.ts`: the read-then-subscribe join, written against a small `Transport` so tests can fake the gateway.
- `src/gateway.ts`: the WebSocket transport and the `join` handshake.
- `src/fold.ts`: the shape of the WebAssembly fold.
- `src/main.ts`: starts the page, runs the gateway loop and dispatches keys.
- `src/state.ts`: the view state, its reducer and the address round-trip.
- `src/views.ts`: filters, the view in the address, and saved views.
- `src/keys.ts`, `src/palette.ts`, `src/list.ts`: the shortcut registry, palette ranking and stage grouping.
  All pure, tested in `test/`.
- `src/ui/`: the views (shell, filter bar, list, board, panel, palette, shortcuts, saved-view dialog, status line), `tokens.css` for colour, space and
  type, and `app.css`.
- `check/`: the headless Chrome check and its fixture log.
- `../wasm`: the Rust crate that wraps `core`'s fold for the page.
- `../wasm/golden`: logs and the state the native fold gives for each. The wasm fold must match them in
  `test/fold.test.ts`, and the native one in `wasm/tests/golden.rs`.
