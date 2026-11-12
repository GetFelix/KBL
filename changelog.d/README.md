# Changelog fragments

Add one file here per change instead of editing `CHANGELOG.md`, so pull requests
stop conflicting on the same lines.

## Naming

`changelog.d/<name>.<section>.md`

- `<name>` is free. Use the issue or PR number and a slug: `212-claim-race`.
- `<section>` is one of `security`, `added`, `changed`, `deprecated`, `removed`,
  `fixed`, `documentation`.

## Body

The entry text, written as it should read in `CHANGELOG.md`. Do not start it with
`- ` or the `Added:` label; the assembler adds both, as in the existing entries
under `## Unreleased`. Continuation lines need no indent. A change that belongs in
two sections gets two files. No headings: `--check` refuses a `#`, `##` or `###`
line.

`changelog.d/212-claim-race.fixed.md`:

```markdown
A release that raced the next claim could evict the new holder. The release no
longer nacks a record that was already handed to the next poller.
```

This becomes `- Fixed: A release that raced ...` under `## Unreleased`.

## Releasing

There is no release workflow yet. At release time run
`python3 scripts/changelog_assemble.py <version> [--date YYYY-MM-DD]`. It appends
the fragments to the `## Unreleased` entries in a fixed section order, retitles
that section `## [<version>] - <date>`, leaves an empty `## Unreleased` above it,
and deletes the fragments. Entries already written in `CHANGELOG.md` are kept.
Commit the result before tagging. A release workflow should refuse to tag while
any file other than this README is left in `changelog.d/`.

CI runs `python3 scripts/changelog_assemble.py --check` to validate names,
sections and non-empty bodies.
