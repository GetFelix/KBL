# Changelog

## Unreleased

- Changed: the crates and binaries are renamed. `felix-mcp` is `kbl-mcp`, and
  `agents-core`, `agents-ingest`, `agents-tool`, `agents-tracker` and
  `agents-wasm` are `kbl-core`, `kbl-ingest`, `kbl-tool`, `kbl-tracker` and
  `kbl-wasm`. The planned reference worker is `kbl-run`. Every `AGENTS_`
  environment variable is now `KBL_` (`AGENTS_TRACKER_URL` is
  `KBL_TRACKER_URL`). For this release the binaries still read an `AGENTS_`
  name when its `KBL_` twin is unset; the fallback goes next release.
- Changed: the OIDC audience of the development IdP, and the gateway's client
  id, is `kbl` instead of `felix-agents`. A build from before this change
  sends the old audience and is refused by the gateway.
- Added: `work_triage`, the tool an agent answers a triage item with
  (`POST /v1/work/triage`). It writes the `triaged` proposal for the item under
  triage: a tier with why, a duplicate, or `unclear`. A person still accepts
  it. Before, an agent could claim a triage item and had no way to answer it.
- Fixed: a release that raced the next claim could evict the new holder. An
  agent that stalled past the visibility and then released nacked a record
  Felix had already handed to the next poller, so that agent was claimed, lost
  its delivery and was lapsed while it worked. A delivery polled before the
  last holder's hand-back ended is no longer turned into a claim. The hand-back
  ends even when the caller's request is dropped, and only the call that
  started it ends it.
- Tracker hardening. An agent can no longer replace a context entry that
  ingest, the fold or a person wrote (`not_your_entry`), and a later holder
  can no longer update an entry a previous agent wrote. The tracker refuses it
  before it is logged; the fold is unchanged, so an old log that holds such a
  replacement folds as it always did. A reported cost saturates an item's total instead of
  overflowing it. The strings a record carries (reasons, imported fields,
  titles, progress branch and pull request, triage notes, a rules target) are
  held to 4 KiB on every route. A ready record that Felix dead-lettered is
  published again after six visibility periods (with growing back-off), so its
  item is claimed again. The empty-lane mark for reviewers is taken before the
  polls, not after.

- Records on `work.events` carry a format, and a running Kibble can be upgraded
  and rolled back without losing work ([docs/upgrades.md](docs/upgrades.md)).
  The tracker pauses for up to 15 seconds while the new one takes the writer
  lease; claims survive it.
  A record's `v` field is its format; a record without one is format 1, and
  format 1 records are written without it, so every existing log reads, writes
  and folds as before. A tracker that meets a record of a newer format than it
  reads refuses to serve and names the offset, and the dashboard page stops
  and asks for a reload; before, both skipped a record they could not decode.
  A new `format_raised` record, which only a person may write, lets records of
  a newer format onto the log once every reader is upgraded. Until then a
  newer tracker writes nothing an older one cannot read. Builds from before
  this change do not stop at a newer record, they misread it, so none may be
  running when the format is first raised. A tracker that cannot read the log
  no longer takes the writer lease, and the tracker refuses to log a record
  of a kind it does not know. This build reads format 1 only. CI runs the previous tracker build and the new one in turn
  over one log.

- Review is claimable work. When an item's pull request opens, the tracker adds
  a first-pass and a final `review` item, each for a reviewer tier
  (`AGENTS_REVIEW_TIERS`, default `fast,deep`, one `work.review.rN` stream
  each). `work_next` takes `review_tiers`, and an agent is never given a
  review of work it did. `work_finding` records a finding (severity, file,
  line, scenario, verdict): a `fix` becomes a child for the author, a `defer`
  a proposal a person accepts. A final review ends with `work_verdict`, merge
  or block, which the dashboard shows and a person acts on. `decide` takes a
  `revert_check`. Every field and count is bounded. Logs from before fold as
  they did; older trackers fold the new records as `unknown`.

- A threat model, [docs/threat-model.md](docs/threat-model.md): actors, assets, trust
  boundaries, and for every tracker route, ingest route, MCP tool and record kind
  the fields an agent controls and the check that stops misuse. CI fails when a
  new one has no row, and the pull request template asks for it.
- The dashboard style frame follows the conventions of Linear, Jira and GitHub
  Projects: a sidebar, list and board layouts of the same items, a side panel,
  a command palette (`Ctrl+K`), `?` for shortcuts, `/` for search, filters,
  saved views, multi-select with bulk actions, undo toasts, loading and empty
  states, and a density setting. The key map changed: `x` selects, `c`
  creates, `s` sets status, `a` assigns, and Kibble's own verbs are numbered
  choices on each card and entries on the palette. Question cards show their
  options and a free text field. `docs/work-tracker.md` has the new table and
  `docs/usability-test.md` the five-person test script.

- Questions can offer options. `work_ask` takes `options` (at most 8) and a
  `default`, and a person picks one or types free text, which is stored as
  written. An answer appends a claimable `resume` item under the asking item
  that carries the question, the answer and the asker's `so_far`; the asking
  item stays out of Ready meanwhile. A person's answer can name another repo
  (`moves_to`) to create a prerequisite there instead. Triage can mark an item
  `unclear` (scope, repo or acceptance), which sends it to a person as a
  question. `work_ask` also takes `cost_micros`, charged when asking. Logs from before fold as they did; older trackers ignore the new
  fields and start no resume work.

- Standing rules per repo and per root: a person sets text and checks-before-push
  with `POST /v1/work/rules` (`rules_set`), and every claim reply and `work_next`
  returns the rules in effect with their version. The root's parts override the
  repo's; agents can read rules but never write them. Older trackers fold
  `rules_set` as `unknown`.

- Ingest keeps one cursor per repo. A repo that answers 403 or 404 stalls only
  itself and is reported in Needs you as `source_stalled`; state saved with the
  single cursor seeds every repo once.
- Ingest records the state of an agent's pull request: new `pr_status` and
  `source_stalled` records, a failed check sends the item to Needs you, and a
  merge closes it as done. The ingest token may now write `closed` with reason
  merged. Older trackers fold the new records as `unknown`.

- `work_post` takes `prerequisite`: work another repo must do first. It creates
  a proposal in the target repo's Triage, links the held item to it, and ends
  the claim as blocked without spending a try. Each link waits for a merge or
  a release (`shipped` record). The token file gains an optional `grants` map
  of the repos an agent may reach; without it an agent reaches only its own
  tree's repo. Logs from before fold as they did.

- The fold caps questions, answers and links per attempt, takes an answer only
  for the open question, and the tools refuse a replayed link, oversized or
  too many evidence entries, an empty `why` and an empty edited title.
- `context_read` needs the item or its parent to be held by the caller.
- Trackers from before the `edited` and `tier_changed` records fold them as
  `unknown`, so run the new tracker before a person edits or re-tiers a proposal.
