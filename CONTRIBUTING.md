# Contributing

[docs/design.md](docs/design.md) describes what is being built.

## Code

- Write the simplest thing that meets the design. Don't add abstractions a milestone doesn't need yet.
- Write the rules once. The record formats, the resource and key names, the task and lease rules and the fold that rebuilds a run at an offset live in `core/`. `felix-mcp`, `agents-run` and `agents-tool` link it and the viewer uses it as WebAssembly. Never copy that logic into TypeScript.
- Felix is the only backend. A feature that needs a database, a queue or a coordinator process of ours is out of scope.
- The model is untrusted. Anything an agent must not be able to do is enforced by a Felix grant on that agent's token, never only by a check in `felix-mcp`.
- Every coordination step goes on the run log, refusals included. If the viewer cannot replay it, it did not happen.
- Tests must run without a model. Every agent role has a scripted stand-in, and CI uses it.
- Plain TypeScript in `viewer/`, no UI framework.
- Format with `cargo fmt` and prettier. Pin dependency versions.

## Comments and docs

- Only comment where the code is unclear, such as a constraint the code can't show.
- Write docs in plain sentences, and update them in the same pull request as the behaviour they describe.

## Pull requests

- CI must pass before merge. Merges are squash only.
- No AI attribution in commits or pull request descriptions.
- List design calls under "Review notes", and Felix gaps you hit under "Felix gaps found", with the issue each one is filed as.
- A new route, MCP tool or record kind gets a row in [docs/threat-model.md](docs/threat-model.md): who can call or write it, which fields an agent controls, and the check that stops misuse. CI fails without the row, and a row with no check needs an issue.
