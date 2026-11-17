# Contributing

[docs/design.md](docs/design.md) describes what is being built.

## Code

- Write the simplest thing that meets the design. Don't add abstractions a milestone doesn't need yet.
- Write the rules once. The record formats, the resource and key names, the task and lease rules and the fold that rebuilds a run at an offset live in `core/`. `kbl-mcp`, `kbl-run` and `kbl-tool` link it and the viewer uses it as WebAssembly. Never copy that logic into TypeScript.
- Felix is the only backend. A feature that needs a database, a queue or a coordinator process of ours is out of scope.
- The model is untrusted. Anything an agent must not be able to do is enforced by a Felix grant on that agent's token, never only by a check in `kbl-mcp`.
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
- A new route, MCP tool or record kind gets a row in [docs/threat-model.md](docs/threat-model.md): who can call or write it, which fields an agent controls, and the check that stops misuse. CI fails without the row, and a row with no check needs an issue. Cite code as `file#symbol` (a function, constant or `Type::method`), never a line, and pin the line it relies on in `docs/threat-model-citations.json`.
- Add a changelog fragment, `changelog.d/<name>.<section>.md`, and do not edit `CHANGELOG.md`: two pull requests that edit it conflict. [changelog.d/README.md](changelog.d/README.md) has the naming and sections.

## Cutting a release

A maintainer tags a commit on `main` and pushes the tag; nothing else publishes. The crates share one version, set
once as `version` under `[workspace.package]` in `Cargo.toml`, and the binaries report it.

1. Bump `version` in `Cargo.toml`, run `cargo update --workspace` so `Cargo.lock` follows, and merge that.
2. Move the `changelog.d` fragments into `CHANGELOG.md` and merge that.
3. Tag and push, with the tag set to `v` plus the version: `git tag v0.1.0 origin/main && git push origin v0.1.0`.

The `Release` workflow builds `kbl-mcp` and `kbl-tool` for five targets (Linux x86_64 and aarch64, macOS aarch64 and
x86_64, Windows x86_64), attests each archive and `SHA256SUMS`, and creates a GitHub release with one archive per
target plus `SHA256SUMS`. It refuses a tag that is not on `main` and a tag that does not equal `v` plus the workspace
version, before it builds anything. The attestations are signed build provenance (GitHub artifact attestations, backed
by Sigstore): `gh attestation verify <file> -R GetFelix/KBL` proves an archive was built by this repository's release
workflow. Only the `attest` job holds `id-token` and `attestations` write.

To rehearse, open a pull request that changes `.github/workflows/release.yml` (it runs as a dry run) or run the
workflow from the Actions tab with `dry_run` on. It builds every target and keeps the archives as run artifacts for 14
days. It creates no release, and a run on a branch cannot create one. A dry run checks the version and builds, but it does
not attest: attestation runs only for a tag.
