# Changelog

## Unreleased

## [0.1.0] - 2026-11-17

- Added: a triage answer can carry `depends_on` (items the root should wait on)
  and `split` (titles it should be cut into), on `work_triage`, `POST
  /v1/work/triage` and the `triaged` record. Both are advice for a person: the
  tracker links nothing and creates no item. Each list holds at most 8
  entries, and a dependency must be another item the agent can see. A
  changed list counts as a different answer toward the 4 per attempt. Records
  without the fields read as before. The tier an admitted root already has is
  documented as untouched by the answer; a person adopts it with a tier
  change. `dev/check-tracker-triage.mjs` runs a scripted triage agent against
  the dev stack in CI.
- Changed: KBL has its own mark, a big square with one bite-sized piece lifted out, in `docs/brand/`. The dashboard frame uses it and switches its accent from Felix cyan to KBL amber. `scripts/render-mark.mjs` replaces `scripts/make-mark.py`.
- Fixed: proposals were limited only per attempt, so an agent that kept claiming
  could keep filling the Triage column. The tracker now refuses a proposal when
  the agent already has 16 waiting for a person, or 8 from work under one
  root. Posted proposals, prerequisites that file work in another repo, and
  deferred review findings all count, and accepting or rejecting one frees its
  slot. A triage holder that kept changing its answer is stopped after 4
  different answers in an attempt; repeating the answer on record is still a
  no-op. The fold itself is unchanged, so old logs replay as before.
- Changed: the pull request branch prefix is `kbl/<item>` instead of `kibble/<item>`, and the bot login in docs and fixtures is `kbl[bot]`. An item claimed under the old prefix will not link its pull request after the upgrade.
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
- Security: An agent can no longer report a pull request another open item already holds by changing the letter case of its owner or repo; the tracker records the casing already in use. Ingest also ignores check runs written by its own GitHub App, as it already did for its own commit statuses.
- Security: The tracker charges every claim end, ask and prerequisite at least a cost floor
  (`KBL_COST_FLOOR_MICROS`, $0.10 by default), lapsed claims included. An agent
  that reports no cost now spends its root's cost budget, so the budget bounds it.
- Security: Evidence on a decision, an answer or a new prerequisite must be a `work.events`
  record of the same tree, or the record that made the citing item, such as the
  finding behind a deferred proposal. The tracker refuses another tree's record
  (`evidence_outside_tree`) and any other stream (`evidence_stream`), so a person
  no longer sees unrelated records presented as support.
- Security: `kbl-tracker` and the `kbl-ingest` webhook serve plain HTTP, so they now refuse a listen address other than loopback unless `--behind-tls-proxy` (`KBL_TRACKER_BEHIND_TLS_PROXY`) or `--webhook-behind-tls-proxy` (`KBL_INGEST_WEBHOOK_BEHIND_TLS_PROXY`) says a TLS proxy is in front.
- Security: The tracker joins an incoming `traceparent` only when the request carries a token it accepts. A request with no token or a wrong one starts its own trace under the tracker's sampler, so it cannot force its spans to be exported. The `mcp.tool` span setup is tested.
- Security: Known secret shapes (GitHub, GitLab, Slack, Google, npm and Stripe tokens, model provider keys, AWS access key IDs,
  JWTs, `Bearer` and `Basic` credentials, `token=` values, URL passwords and PEM private keys) are replaced with
  `[redacted]` in the free text of every record an agent writes and of a person's note or answer, by the tracker before
  the append and by `kbl-mcp` before it stores long text, a task result, a task's title or reason, or the error text of a
  `refused` run log record. Each is matched on the real format, so branch names, repo names and code that only look
  alike are kept; a progress record's `pr` and `branch` lose only URL userinfo. Secrets with no prefix, such as AWS
  secret access keys, are not caught.
- Added: Ingest can sign in as a GitHub App. `github.app` takes the App id, an installation (by id, repo or organization) and the private key file. Ingest signs the JWT, mints installation tokens, keeps each until five minutes before it expires, and mints again once if GitHub answers 401. `token_file` still works and wins when both are set; `token_env` comes last. The key file must be mode 600 or 400. A reinstalled App is found again after a 404 or 401 on the old installation id. Ingest warns when the API base is not https or when several credentials are set.
- Added: The replay corpus in `wasm/golden` now has a log for format 9 (`format_9`) and for the logs written before reviews (`pre_review`), and a test fails if a format or an old log is missing from it. CI fails a pull request that changes a stored fold result there without a changelog fragment naming it.
- Added: A seeded fault sweep runs the tracker's claim path and ingest under crashes between writes, repeated deliveries, broker failover and save conflicts, and checks that an item is worked once, no upstream issue is lost and no dismissed root reopens. CI runs 400 seeds per pull request and 5000 nightly; see `docs/fault-sweep.md`.
- Added: `kbl-tool work export <file>` writes `work.events` to a checksummed backup, `work import <file>` loads one into an empty log or carries on after the start of the same backup, and `work digest` prints a digest of the log's fold (`--backup` digests only what the file covers, `--state` also writes its canonical text). `org export`, `import` and `digest` do the same for the organization's log, so a team-mode tracker restores with its teams and token hashes. A Raft log's blank offsets are kept and restored. Backup files are readable by their owner only. CI backs up a filled team-mode stack, restores into a fresh one and compares the logs. See docs/backup-restore.md.
- Added: kbl-tool prices import takes --team (KBL_TEAM) so a person token in more than one team can set prices; without it the tracker answers team_required.
- Added: The tracker, `kbl-ingest` and `kbl-mcp` serve Prometheus metrics on a port of their own, off until `--metrics-listen` (`KBL_TRACKER_METRICS_LISTEN`, `KBL_INGEST_METRICS_LISTEN`, `KBL_MCP_METRICS_LISTEN`) is set: request count and latency per route and status, claim latency and counts, completions, releases and give-ups, fold lag against the Felix stream, ingest poll results, errors and lag per source, and MCP tool calls, latency and errors. A new `kbl-metrics` crate holds the shared recorder and listener. Labels hold route templates, tool names and result words, never item text, names or tokens. See docs/metrics.md. Traces, an SLO page and runbooks are still to come ([#127](https://github.com/GetFelix/KBL/issues/127)).
- Added: An SLO page with proposed targets for claim latency, ingest lag and dashboard freshness, example Prometheus alert rules and a runbook per alert ([observability.md](docs/observability.md)). Traces are not wired yet ([#127](https://github.com/GetFelix/KBL/issues/127)).
- Added: A version tag now also publishes `ghcr.io/getfelix/kbl-tracker` (with `kbl-ingest`) and `ghcr.io/getfelix/kbl-dashboard` for amd64 and arm64, with build provenance attestations; a manual dry run builds them and pushes nothing. A Helm chart in `deploy/helm/kbl` installs the tracker (one replica), the dashboard and optionally ingest with non-root, read-only, no-capability pods, secrets only from Secrets, and optional NetworkPolicy and Ingress. Felix is not in the chart. CI lints and renders the chart. See docs/deploy.md ([#128](https://github.com/GetFelix/KBL/issues/128)).
- Added: Ingest has a GitLab source. A `gitlab` worker reads the issues of a configured set of projects on GitLab.com or a self-managed instance, picked by label, milestone, assignee or type, maps priority labels to tiers, turns "is blocked by" links into edges, and reads the merge request an agent reports with the same checks as a GitHub pull request: our own login opened it, it came after the first claim, it is tied to the item by branch or a closing reference, and it merged into the default branch by someone other than us. It signs in with a group or project access token (`read_api`, and `api` only where a rule writes back), refuses plain http except on loopback, and writes back a comment and a close or reopen only where a rule asks. `gitlab` is a known source for `ingest_sources` in the token file.
- Added: The dashboard has a board (`V` switches list and board; `H`/`J`/`K`/`L` move), filters by source, tier and repo from what the log recorded, views in the address (`view=blocked`, `layout=board`, filters and search) that can be saved by name to the sidebar, with the Blocked view composing with all of them and board cards saying what a blocked item waits on, a compact density, and a Menu button on small screens. The WebAssembly fold's item list now carries each item's recorded tier, source and repo. An older `?view=board` link or saved view still opens the board and is rewritten to `?layout=board`. Filter selects with no value now show Any instead of a blank.
- Added: The dashboard groups items by stage, opens one in a side panel, and has search, a command palette (`Ctrl+K` or `Cmd+K`), `?` for shortcuts and `J`/`K` movement. Loading, empty, no-match and stopped states say what to do next. It also fixes the built page never leaving "Reading history": the production bundle dropped the call that hands gateway messages to the follow loop.
- Added: The dashboard writes: a person signed in with their own tracker token can accept, reject, set the tier of, close and answer items through the tracker's person routes, from the panel or in bulk (`X` marks an item, each list row has a checkbox). Each write shows as pending, written or refused with the tracker's reason. A tier change can be undone (`Mod+Z`) with a new `tier_changed` record; the writes the log cannot take back ask to confirm first. A Needs you view (`?view=inbox`) lists what waits on a person and why, and an open question shows its text, options and default. The page calls only its own origin's `/v1` routes (`npm run dev` forwards them to `KBL_TRACKER_URL`). The WebAssembly fold's item list now carries each item's `needs`, open question offset, options and default, and whether it is a root.
- Added: A link holds an item back across repos and sources. The core fold names each blocker (`State::blockers`), and the dashboard's Blocked view lists only items in the Blocked stage, not triage or held ones. Each blocker shows its repo when it is a different one, else its source (such as `jira`), and "release" when it is merged and the link waits for the release. Only GitHub items name a repo for display (`State::repo_of`); an Azure DevOps `org/project` still drives rules and spend but is not shown as a repo.
- Added: `kbl-tool work spend` prints what claims cost, summed per root, reviewer tier and repo. The numbers come from the fold, so no new record and no format raise; the wasm fold exposes the same numbers via `Folder.spend_json()`. Spend is a lower bound, because a lapsed claim records no cost.
- Added: Overlapping files. An item can declare the files it expects to touch in a
  `files` context entry. When two open items share a path, the dashboard puts a
  "1 file overlap" flag on both rows, and the item panel's "Same files" section
  names the other item (a link) and the shared files and suggests which should
  wait on the other. The warning is derived from the fold, so it adds no record
  and no format raise.
  Only items in the same repo are compared, held work is suggested before work
  in review and that before work not yet started, and only `./`, `/` or `.`
  names the whole repo.
- Added: Ingest writes `shipped`. When an open item's link waits on a merged root for a release, ingest records the oldest stable release after the merge whose tag contains the merge commit, which frees the link. The tracker takes it only on a root ingest imported whose reported request has a merged status, and never over a release already recorded. Each poll makes at most `release_compares_per_poll` pull request reads and compares together (default 20) and reads at most 5 pages of releases; a merge older than those pages gets no release rather than a possibly wrong one, and a tag found not to contain the merge is not compared again. A person's `shipped` replaces ingest's. Only roots something waits on cost any release requests. The ingest GitHub token needs read access to repo contents. Part of #144.
- Added: Ingest opens the upstream issue for a prerequisite a person accepted, when the
  target repo's intake rule sets `open_prerequisites: true`, and ties the item to
  it so upstream sync follows it like any imported root. The tie is format 6: a
  format 5 log and older builds are unchanged. A crash between opening the issue
  and recording it is finished by the next poll, which finds the issue by its
  `kbl-prerequisite:` marker and opens no second one. The waiting issue is named in
  the new one only when the rule sets `link_waiter: true`.
- Added: Format 3: triage posts prerequisites. An agent holding an item's triage work
  can call `work_post` with `work: "prerequisite"`, and the item under triage
  waits on that work in another repo before any build claim is handed out. The
  triage claim goes on and nothing is charged by the call. The tracker refuses
  it with `format` until a person raises each team's log to format 3
  (`format_raised`, see docs/upgrades.md), after every tracker and dashboard
  page reading that log is this build or newer.
- Added: Run log records and tasks carry a format (`v`, left out at format 1, so
  nothing written changes). A worker that claims a task of a newer format hands
  it back with an error instead of running it without the part it cannot read,
  and `record::read` tells a reader of a run log when a record is newer than its
  build. docs/upgrades.md says why the cache values need no format.
- Added: A work loop for MCP hosts. The `work_next` reply carries a `loop` line (and one when nothing is ready) telling the host to keep taking items, and the README has the prompt. `dev/agents.mjs` starts N host sessions, each with its own `.mcp.json` and tokens, restarts one that exits early, and stops them all on Ctrl-C. `dev/check-agents.mjs` runs it against the dev stack with a scripted host.
- Added: `kbl-tool agent add <name>` mints a tracker token for a new agent, one per `kbl-mcp` session, and the tracker accepts it on the next call with no restart. `agent list` shows each agent and what it holds, and `agent revoke` stops the token and releases the agent's holds with the reason `revoked`, as a lapsed claim is. The routes are `POST`, `GET` and `DELETE /v1/teams/{team}/agents`, for org admins, with teams on. Only the token's SHA-256 is kept, on `org.events`.
  A team holds at most 256 live agents; a revoked agent frees its place but not its name. While a tracker does not hold the org lease it answers `503` with `org_unavailable` to agent tokens added this way, and it rereads `org.events` when it takes the lease back, so a revoke written meanwhile holds. The tracker refuses to start while its token file names an agent `agent add` minted and nobody revoked.
- Added: Teams, third step (#185). With `KBL_ORG_NAMESPACE=kbl` the tracker reads `org.events`, adopts `KBL_NAMESPACE` as the first team when the log is empty, and serves every team the log lists. `kbl-tool org init` creates the org namespace, `org.events` and the tracker's Felix role; `kbl-tool team create` creates a team's namespace and roles and calls the new `POST /v1/org/teams`, which starts serving the team with no restart (`GET /v1/org/teams` lists them). Org routes take a token-file person, who is an org admin. A team that loses its lease, or whose log has a record of a newer format, answers 503 with `team_unavailable` and is started again in the background while the others serve (#195).
- Added: The `org.events` records and their fold in `kbl-core`, the first step of teams (#185): teams, people, roles, agents, sources, ingest workers and grants, with token hashes in place of tokens, the team and name rules, and golden logs. Nothing serves teams yet.
- Added: The tracker keeps its fold, writer lease and Felix connections per team behind a team table, the second step of teams (#185). Every work and ingest route is also served under `/v1/teams/{team}/`, and must name a team the token belongs to (`404` with `unknown_team` otherwise). `GET /v1/me` answers a person's teams and role in each. Each team takes at most `KBL_TEAM_INFLIGHT` calls at once (64), and a team that loses its lease answers 503 alone. There is one team, `KBL_NAMESPACE`, with the token file's callers.
- Added: Format 2 and its first record, `returned_to_scope`. When an issue that left its
  intake rule matches it again, ingest writes it and the item leaves Needs you for
  the stage it had; a person can write it too, to keep an item whose issue no
  longer matches. The tracker refuses it until a person raises each team's log
  to format 2 (`format_raised`, see docs/upgrades.md), and only after every
  tracker and dashboard page reading that log is this build or newer. Until the
  raise nothing changes: a returning item stays in Needs you.
- Added: The dashboard has a Spend dialog (sidebar button, or `Show spend` in the palette) listing what claims cost per root, reviewer tier and repo, largest first, and a By model table with each model's unit, usage and cost, from the fold's `spend_json()`. A model with no price shows as unpriced with its usage, never as $0. It says the total is a lower bound: lapsed claims and unpriced models are charged only the cost floor, and reported usage is the agent's own word. No new record and no format raise.
- Added: `kbl_core::work_fold::fold_to` folds `work.events` to any offset, skipping the blank offsets a Raft shard leaves, and `Snapshots` keeps a checked copy of the state every 2,000 records so a late offset folds quickly. A snapshot is checked by a hash chained over every record it covers; one that does not match the log is skipped and rebuilt.
- Added: A non-terminal write by an agent or a person (a note, a post, an ask, an edit, a link, a decision and the rest) takes an optional `request_key`. A retry with the same key and body writes nothing and answers with the first write, `replayed: true`, including after the tracker restarts. The same key with another body is `409` `request_key_reused`. Each caller's last 256 keys are remembered. The key is a new optional `req` field on the record, which readers without it ignore. The `kbl-mcp` write tools take `request_key`.
- Added: `kbl-tool prices import <file>` writes `price_set` records from a price table and
  skips prices already in effect; `--update` writes one that differs, `--dry-run`
  only says what it would do. `--default` imports the table shipped with the tool
  (`tool/prices/default.json`: dated Anthropic list prices, to be checked against
  the vendor's page), so a fresh install is not left with every model unpriced.
- Added: The dashboard's Spend dialog lists each model and unit waiting for a price in a card, where a
  signed-in person can set that unit (it is not a Needs-you item and blocks nothing), and
  shows "check pricing" next to a model whose oldest price in effect is more than 90 days
  old, counted from the import's `as_of` day (`?price_age_days=N` changes the age). A new
  Models and prices page, from Spend or the palette, lists every model seen in recorded usage
  with each unit's price, start, source, usage and history, and lets a person set or change a
  price (a change in effect shows old to new and needs a confirm). The spend view gains
  `prices` (every price set) and `unpriced` now names the unit. `price_set` carries an
  optional `as_of`, which `kbl-tool prices import` fills from the table; a price with it is
  format 9, one without stays format 8. The page asks again if the price changed under an open
  form, and keeps focus and the saved message through a save.
- Added: An item can have an assignee. A person takes an item (`POST /v1/work/take`), assigns it to a person of the team or to nobody (`POST /v1/work/reassign`, `{item, to}`, `not_member` for someone outside the team), and sends a note to the agent holding it (`POST /v1/work/steer`, `{item, attempt, text}`), with new records `taken`, `reassigned` and `steered`. They are format 12: the tracker refuses them until a person raises each team's log to 12 (`format_raised`, see docs/upgrades.md), after every tracker and dashboard page reading that log is this build or newer. A person assignee keeps an item out of Ready: where it would be Ready it waits in Needs you as `taken`, and an agent already holding it keeps the hold. An agent assignee is refused as `bad_assignee` for now (#324).
  My queue is per person: `GET /v1/work/queue` leaves out items assigned to someone else, and each entry, and each `GET /v1/work/items` row, names its `assignee`. `kbl-mcp person` adds `take_item`, `reassign_item` and `steer_agent`. The wasm rows carry `assignee` and `held_attempt`, and the golden `format_12` pins the new records and the state they fold to.
- Added: A person can request a review, send a pull request back for changes, record a merge and mark an agent's summary accurate or not over REST: `POST /v1/work/request-review` (`{item, reviewer}`), `/request-changes`, `/mark-merged` and `/rate-summary` (`{item, attempt, accurate}`), with new records `review_requested`, `changes_requested`, `merge_marked` and `summary_rated`. They are format 11: the tracker refuses them until a person raises each team's log to 11 (`format_raised`, see docs/upgrades.md), after every tracker and dashboard page reading that log is this build or newer. The ask and the send-back sit beside upstream's `pr_status` and drop when upstream answers or the request ends; a recorded merge closes the item as merged; the latest summary mark per attempt stands.
  The dashboard's Story section shows each finished attempt's summary mark with Accurate and Not accurate buttons. The name `summary_rated` is now a known record kind: an old unversioned `summary_rated` line no longer decodes as unknown. The wasm goldens `format_10` and `format_11` pin the new records and the state they fold to.
- Added: `kbl-mcp person` serves a person's card actions and My queue as MCP tools: `my_queue`, `accept_proposal`, `reject_proposal`, `answer_question`, `set_tier`, `flag_item`, `unflag_item`, `add_note`, `retry_item`, `snooze_item`, `park_item`, `unpark_item` and `unblock_item`. It runs as a separate process with a person's token in `KBL_PERSON_TOKEN` and refuses to start with an agent's token, so an agent never gets these tools. The tracker still checks every route and records the person.
- Added: The tracker can keep fold snapshots on disk (`KBL_TRACKER_SNAPSHOT_DIR`) and start from the newest one that matches `work.events`, folding only the records since. A damaged, stale or other-revision file is removed and an older one tried, then a full fold. A snapshot now also keeps each pending proposal's agent and root, which a restored state had lost, and the fold revision is 3.
- Added: The tracker, `kbl-ingest` and `kbl-mcp` export OpenTelemetry traces over OTLP/HTTP, off until `OTEL_EXPORTER_OTLP_ENDPOINT` (or the traces-specific variable) is set and configured by the standard `OTEL_*` variables. Spans cover tracker requests (named by route template), claims, fold applies and rebuilds, ingest polls and write-back, MCP tool calls and their calls to the tracker, and a W3C `traceparent` joins each caller to the tracker request. Attributes hold templates, tool names, source kinds and result words, never ids, text or tokens. A new `kbl-trace` crate holds the setup. See docs/observability.md. This completes [#127](https://github.com/GetFelix/KBL/issues/127).
- Added: The GitHub worker now records the state of an agent's pull request on a Jira root, so a Jira rule's `transition_on_merge` fires when the request merges. Token files can tie an ingest worker to one source with `ingest_sources`; a tied worker reaches only that source's roots, except that the `github` worker may write `pr_status` on any root. The startup warning about `transition_on_merge` is gone. A tied worker other than `github` lists only its own source's roots, and the tracker refuses a token file that ties some workers and not others, names an unknown source, or has a key it does not know. An older tracker ignores `ingest_sources`, so downgrading removes the ties.
- Added: The running tracker now saves a fold snapshot every `KBL_TRACKER_SNAPSHOT_EVERY` records (2,000 by default), on a blocking thread from a copy taken at a record boundary, extending the hash chain from the last save instead of hashing the whole log. Before, snapshots were written only by a rebuild, so a tracker that stayed up never refreshed them. A rebuild still checks every file against the log and skips one that does not match, and a save after retention trimmed the log is no longer pruned for its low record count.
- Added: The tracker serves trust numbers from the summary marks people write: `GET /v1/work/metrics/summary-trust?days=` (a person only, 1 to 366 days, default 30) counts the marks written in the window, how many were accurate, and the finished attempts with no mark. An attempt counts once, by its latest mark, and the numbers name no agent, model or reviewer. The dashboard's new Metrics dialog shows them with the counts first, and says so when fewer than 10 marks are too few to read a trend from. The fold keeps each mark's time outside its wire form, so snapshots are rebuilt once (the fold revision is now 4). The summary mark paragraph in docs/decision-replay.md is true again.
- Added: `kbl-mcp` renews its Felix token from an ID token, so a 15 minute token no longer ends an agent's session. Set `KBL_FELIX_ID_TOKEN_FILE` (mode 600) or `KBL_FELIX_ID_TOKEN_COMMAND` with `KBL_FELIX_CONTROL_PLANE` instead of `KBL_FELIX_TOKEN`. It renews on a timer and reconnects to Felix after each renewal, so forwarded writes carry a live token. A held claim keeps standing across a renewal, and a failed renewal keeps the current token until it expires. `KBL_FELIX_TOKEN` still works as a fixed token. The quick start's `dev/connect.mjs` now writes the agent's ID token to `dev/state/<agent>.id-token` and a config that renews from it; `--fixed-token` writes a fixed Felix token as before.
- Added: Release archives and `SHA256SUMS` carry signed build provenance (`gh attestation verify <file> -R GetFelix/KBL`). The crates are at 0.1.0, set once in the workspace, and the release workflow refuses a tag that does not match it (#318).
- Added: `kbl-ingest check --config ingest.json` validates a config with the worker's own parser, tests each credential, reports the permissions every rule needs, and dry-runs the first poll to list what each rule would import (first matching rule wins, caps applied). It only reads: the vendor connection refuses any request but a GET. `--json` prints the report, and the exit status is 1 when anything is an error. `kbl-tool source init github|jira` writes a starter config. [docs/ingest-setup.md](docs/ingest-setup.md) is the operator guide. The quick start has an optional ingest step: `dev/ingest.mjs` writes a config for one repository and label, runs `check`, then runs ingest against the local tracker. `check` now fails a `token_file` or `token_env` credential that GitHub answers 401, 403 or 404 on `/user`; only a configured `app` treats 403 or 404 there as an installation token.
- Added: An item can be assigned to an agent: `POST /v1/work/reassign` with `{"to": {"agent": "w1"}}`, or `kbl-mcp person` `reassign_item` with `to: "agent:w1"`. Only that agent can claim it; the fold refuses any other as `not_assignee`. The tracker hands the item straight to its assignee in `work_next` and never publishes it to a ready lane, so other agents never hold it, never loop on it, and spend none of Felix's handouts on it. An agent assignee that is revoked or makes no call for `KBL_ASSIGNEE_AWAY_SECS` (600 by default) is dropped by the tracker, and the item goes to the team. This is format 13 (no new record): raise each team's log to 13 after every tracker and dashboard page is this build or newer (docs/upgrades.md).

  The new `format_13` golden folds a log that assigns an item to an agent, refuses another agent's claim, and lets the assignee claim it.
- Added: `kbl-ingest check` now tests Azure DevOps and GitLab configs instead of only validating them. It signs in, reports the permissions each rule needs (project read, work item edit, the GitLab `api` scope and role, token scopes and expiry), and dry-runs the first poll the way the worker selects it. Everything it sends is a read; for Azure DevOps the one allowed POST is the work item query.
- Added: Org format 2: an org admin raises `org.events` with `format_raised`, then sets a team's connections (`connection_set`: worker group, budget pool and served sources) and pool budgets (`budget_set`) through `POST /v1/org/records`. Ingest workers read their group's connection and budget share from the `GET /v1/ingest/config` long poll (`ETag`, `If-None-Match`, `wait` up to 60 s). A worker whose group has no connection stays file-managed.
- Added: Ingest source leases: `PUT /v1/ingest/leases` renews a worker's leases and takes the sources rendezvous hashing gives it once they are released or 30 s past their 90 s expiry, raising the epoch on each take. Group membership is kept in the cache, so every tracker assigns the same way. A worker with `tracker.group` set holds the config long poll on its own task, renews every 30 s, polls only the sources it holds, and makes no upstream request once its last successful renew is 60 s old by its monotonic or wall clock, cancelling any request still running. A stopping worker stops its heartbeat before it releases its leases. The config answer lists the worker's assigned sources and leases when it names its `instance`. A leased worker never polls a configured repo its connection does not name, and warns about it. Comment write-backs carry a hidden mark so a retry after a cancelled request does not post twice. A worker run that left is not let back in by a late renew.
- Added: A `v*` tag now builds `kbl-mcp` and `kbl-tool` for Linux (x86_64, aarch64), macOS (aarch64, x86_64) and Windows (x86_64) and publishes them with `SHA256SUMS` as a GitHub release. The release workflow also runs by hand as a dry run that keeps the archives and creates no release ([getting the binaries](README.md#getting-the-binaries)).
- Added: A quick start: `dev/quickstart.sh` runs Felix, felix-gateway, the tracker and the dashboard locally in containers built from the checkout (Docker or Podman), all on 127.0.0.1, and `node dev/connect.mjs` writes an `.mcp.json` that points Claude Code at `kbl-mcp` with absolute paths and mode 600. It is a single-user stack with a stand-in identity provider, not a deployment. CI runs it on a clean runner.
- Added: People can read the tracker over REST with their own token: `GET /v1/work/items` (optionally `?stage=`), `GET /v1/work/items/{item}` and `GET /v1/work/items/{item}/history`, which pages through the records about one item. `POST /v1/work/flag` and `/v1/work/unflag` move an item into or out of Needs you with a new `flagged` record, the first record of format 7: the tracker refuses it until a person raises each team's log to format 7 (`format_raised`, see docs/upgrades.md), after every tracker and dashboard page reading that log is this build or newer. Logs that never raise fold as before.
- Added: A dev-stack check, `dev/check-tracker-ingest.mjs`, runs `kbl-ingest` against a stand-in GitHub: an issue polled three times, and again after ingest loses its saved state, is imported and admitted once, with one triage and one augment item.
- Added: Ingest has an Azure DevOps source. An `ado` worker reads the work items of an organization's projects, picked by tag or by a saved query, maps Priority 1 to 4 to tiers, turns predecessor and child links into edges, and maps an area path to the repository agents work in; an item in an unmapped area waits in Needs you. It signs in with a personal access token or a Microsoft Entra service principal, and writes back a comment, a pull request hyperlink and a state change only where a rule asks. `ado` is a known source for `ingest_sources` in the token file.
- Added: Ingest can read a Jira site: a rule's JQL filter selects issues, priority maps to tiers, `Blocks` links and sub-tasks become edges, and an issue whose component maps to no repository waits in Needs you. Write-back adds a remote link and a comment for an agent's pull request and applies a configured workflow transition. Sign in with an API token or an OAuth access token held by ingest only. A worker serves one source, so `github` and `jira` are exclusive in its config.
- Added: `POST /v1/work/note` lets a person add a note, review comment, answer or priority change to an item as a
  `human_input`, the record a redo names as its note.
- Added: The dashboard's side panel shows an item's story: its recorded decisions in order across attempts, grouped understand, reproduce, cause, fix and verify, with the edits and cited steps behind each and deterministic flags where a claim and the recorded steps disagree. The fold behind it is `kbl_core::story`.
- Added: A person can retry, snooze, park, unpark and unblock an item over REST: `POST /v1/work/retry`, `/snooze` (`{item, until_ms}`), `/park`, `/unpark` and `/unblock` (`{item, to}`), with new records `retried`, `snoozed`, `parked` and `unblocked`. They are format 10: the tracker refuses them until a person raises each team's log to 10 (`format_raised`, see docs/upgrades.md), after every tracker and dashboard page reading that log is this build or newer. A snoozed or parked item leaves `GET /v1/work/queue` while the wait it was hidden for continues.
  A park covers only the wait it was set on, and a snooze or park records the reason it covered, so a wait for another reason shows the item again even when its start is unknown. The name `snoozed` is now a known record kind: an old unversioned `snoozed` line carrying other fields no longer decodes as unknown.
  The wasm golden `format_10` pins the new records and the state they fold to.
- Added: Added `GET /v1/work/queue`, My queue as a read: every item waiting on a person, oldest wait first (waits begun before a snapshot first of all), then tier. `?limit=` defaults to 200, at most 1000.
- Added: A review limit. `KBL_REVIEW_LIMIT` (default 3, 0 is off; the default is a guess) pauses new fix claims while In review plus fixes agents hold reaches that number. It never blocks triage, questions, reviews, resumed work or fixes a review asked for, and the dashboard header says "review full: agents paused, N/N" with the split when the review load is at the limit. `GET /v1/me` carries `review_limit` per team.
- Added: Time to first human action is measured: the fold records when an item starts waiting on a person (a proposal, a question, a failure, a PR in review) and which person record ends it, from existing records only, and `GET /v1/work/metrics/time-to-human?days=` returns the team's count, p50 and p90, per reason, with the open waits reported apart. A state restored from a snapshot has not seen the waits before it, so `truncated` is true for windows reaching back before its first later record. The dashboard has no Metrics tab yet, so this is REST only.
- Added: The dashboard has Flag for a person and Unflag buttons on the item panel and the bulk bar, and palette commands for both. The item list from the wasm fold now carries `accepted` and `flagged`.
- Changed: `work_post` with a key that already made an item answers with the item's `stage` and `closure`, so a call that hit a closed or rejected item can tell.
- Changed: An issue closed with no `state_reason` (GitHub leaves it empty on issues closed before it kept one) is now completed instead of dropped. Other unknown reasons are still dropped.
- Changed: Dashboard: stronger control edges (3:1 in both themes), a contrast test over the colour tokens, landmark, live-region and Tab-order checks, and a performance budget for a 10,000-item log, all checked in CI (#131).
- Changed: A redo of an item whose pull request has a failed check must name a note
  written after the failure. The tracker answers `note_required` otherwise, so
  a failed check is not rerun before someone has read it.
- Changed: A grant in the tracker's token file names a source as well as a repo, as `source:owner/repo` (for example `github:GetFelix/felix-gateway`). A prerequisite or a linked item must match both, so a grant no longer reaches a repo of the same name in another source. A bare `owner/repo` grant is refused at startup.
- Changed: `scripts/check-threat-model.mjs` now finds routes and MCP tools in every workspace crate, needs a row for each method of a path served with more than one, and pins every `file:line` the threat model cites to the text of that line (`docs/threat-model-citations.json`), so a citation that drifts fails CI. `--fix` moves a citation whose line moved. The threat model's citations were repointed at the lines that do each check.
- Changed: The warn colour is now a yellow that stays apart from the KBL amber accent: `#6E5F00` in the light theme (6.3:1 on white) and `#FFD04D` in the dark theme. Screenshots that show warn states are re-shot.
- Changed: The tracker asks Felix for tokens narrowed to its team's namespace (`resources` on the token exchange), so the broker refuses a write that reaches another team's streams. Felix's control plane must support `resources` narrowing, as 0.6.2 does.
- Changed: The tracker refuses to start when `KBL_NAMESPACE` is `kbl` or does not follow the key rule, since the namespace is now a team's name. A team over `KBL_TEAM_INFLIGHT` calls in flight (64 by default; waiting `work_next` calls have a pool of the same size of their own) answers `503` with `team_busy` and `Retry-After: 1`; raise it for a team with more agents.
- Changed: The agent launcher backs off idle sessions: the wait before asking again doubles from `--idle-delay` up to `--idle-max` (300 s) and starts over after a run of a minute or more, so an idle session stops spending model tokens every 15 seconds. A launcher killed with SIGKILL no longer leaves its 0600 config directory in tmp; the next one removes it.
- Changed: An issue reopened after ingest forgot it, while no intake rule matched it, now
  takes the first rule that matches it later (if that rule is under its cap) and
  comes back into scope. Before, it had no rule and stayed out for good.
- Changed: Agents report spend as `usage` per model, in tokens (input, output, cache read,
  cache write) or AI credits, instead of a `cost_micros` they worked out
  themselves. The fold prices usage with the price in effect when it was written,
  from `price_set` records a person writes with `POST /v1/work/prices`; budgets and
  the floor are in the same derived cost. A model with no price is charged the floor
  until a person prices it, then the rest is drawn. A report that still sends
  `cost_micros` is refused with a message that says to send `usage`. This is log
  format 8: until a person raises the log, reports are charged the floor and usage
  is dropped. Spend gains `by_model` and `unpriced`.
- Changed: POST /v1/work/prices takes an optional `expected_price` (`price_micros` and `since_ms` of the price being replaced, or `{"none": true}` for a first price). When what is in effect when the new one starts differs, the tracker answers 409 `price_changed` and writes nothing, so two people changing the same price cannot overwrite each other unseen. Without the field nothing is checked. The dashboard sends the price it showed, or none, and asks for a review on a conflict, and `kbl-tool prices import --update` names the price it replaces, or none for a new one.
- Changed: The dashboard's review-limit header shows the review load the tracker's pause uses, such as "review full: agents paused, 3/3: 2 in review, 1 in progress", instead of counting only In review.
- Changed: The threat model cites code as `file#symbol` instead of `file:line`, and `docs/threat-model-citations.json` pins the lines each symbol must still hold. A merge that shifts lines no longer conflicts other pull requests; a renamed, removed or ambiguous symbol still fails the check. `--fix` is gone.
- Changed: The tracker refuses to start unless `work.events` is acknowledged durably: a durable stream that is `Quorum`, or has one copy. Its claim checks trust that Felix never cuts a record it acknowledged, and the error names the settings it found. The dev stack's broker now fsyncs before it acks.
- Changed: `decide` in `kbl-mcp` refuses a blank `so_far`, why or commit before calling the tracker, with the tracker's own `refused: missing_field` for a blank `so_far` or commit, and its description tells the
  agent to commit its work first, since a redo starts from that commit.
- Changed: Pull requests add a file under `changelog.d/` instead of editing `CHANGELOG.md`, so they stop conflicting on the same lines. `scripts/changelog_assemble.py` folds the fragments into `## Unreleased` at release time, and CI validates them.
- Fixed: The tracker keeps at most 8 MiB of context, note, question and answer text in memory and reads the rest back from the log on demand.
  When retention has trimmed the text an ingest context entry would be compared with, ingest writes the current text instead of skipping it.
- Fixed: Proposals waiting for a person count against the root's remaining work budget: a root refuses another as `over_budget` once they fill it, and a person's accept or reject frees the room.
- Fixed: Ingest forgets closed, settled issues from its saved state, so the state no longer grows toward the 4 MiB limit and fails with `too_big`. A pruned issue that is reopened upstream still reopens its root.
- Fixed: Whether ingest closed a root is now read from the log (`closed_by_sync` on `GET /v1/ingest/roots`) instead of ingest's saved state, so a close whose save failed or whose poll crashed no longer stops the root reopening with its issue.
- Fixed: Comments made on an issue while it waited over its rule's cap are imported when the issue is admitted. They were dropped before.
- Fixed: The tracker renews its writer lease on its own timer, so a sweep stuck on a slow Felix call no longer lets the lease run out, and each lease put or read gives up after 3 seconds instead of 10. A renewal refused while the lease entry cannot be read no longer fences the tracker. An extend that Felix refuses as `stale_claim` now answers `lost` only after the tracker confirms it still holds the lease; otherwise it answers 503, since a broker whose clock runs fast may have let another tracker take over the claim.
- Fixed: A tracker restart no longer costs every Ready record a delivery attempt per holder, so restarts stop dead-lettering the backlog. A poll that fails while the restarted tracker takes back a delivery now keeps the hold and is tried again, instead of freeing a hold the agent is still extending; such a hold is freed one visibility period after the first failure.
- Fixed: A `complete`, `release`, `give_up` or prerequisite post retried after its record landed (the tracker died before answering, or the answer was lost) is now answered with what was written, `replayed: true` and the record's offset, instead of `lost`. `kbl-mcp` sends the claim's offset (`claim`) and `attempt` with these calls; a retry is replayed only for that claim and a record the agent wrote itself, so a claim the tracker released as lapsed is still `lost`, and a retry never ends a newer claim the agent took on the same attempt. The `claimed` answer and the terminal answers now carry the record's `offset`.
- Fixed: The backup tool waits out a shard with no leader on every read, and the CI backup check waits for each log to be readable before importing.
- Fixed: Ingest reads every page of a pull request's reviews. Before, it read the
  first 100, so a request with more could show as approved after a later review
  asked for changes.
- Fixed: Format 5: a prerequisite may name an existing item that is merged or completed
  but not yet released when it waits for the release (`done_when: released`).
  Before, it was refused as `closed`, so the agent had to post a duplicate new
  prerequisite. A later prerequisite on the same link that asks only for the
  merge no longer drops an earlier ask for the release. Re-opening a
  prerequisite keeps its release waits at every format and clears its release, so
  the waiter waits for the next one. Format 5 adds no record;
  it changes how the fold reads an existing log, so it applies only after a
  person raises each team's log to format 5 (`format_raised`, see
  docs/upgrades.md), once every tracker and dashboard page reading that log is
  this build or newer.
- Fixed: `kbl-mcp` shows a task body stored as an artifact only if its bytes hash to the artifact id and fit the size limit, as it already did for other artifacts.
- Fixed: The tracker ends a hand-back even when the task sending the nack panics. Before, the item stayed unclaimable until the tracker restarted.
- Fixed: A root that ingest imported or reopened, whose issue closed before ingest's save landed, is now closed on the next poll instead of staying open for good. `GET /v1/ingest/roots` serves `reopened_by` (a person or the tracker), read from the log, and ingest keeps an open root with a closed issue only when a person reopened it.
- Fixed: A transferred issue that was closed or reopened after the poll that saw the
  move now closes or reopens its item. Before, ingest lost the item's root under
  the new id on the next poll or restart and ignored the change.
- Fixed: Format 4: a redo of an item whose pull request has a failed check is handed
  out once the item has a note written since the failure, and a released or
  lapsed claim on it can be retried. If the checks fail again, a new note frees
  it; no new redo is needed. Before, the item stayed in Needs you while the check was red, so the redo
  waited for green and its claim was refused with `not_ready`. Format 4 adds no
  record; it changes how the fold reads an existing log, so it applies only after
  a person raises each team's log to format 4 (`format_raised`, see
  docs/upgrades.md), once every tracker and dashboard page reading that log is
  this build or newer.
- Fixed: Ingest now writes `unlinked` when a blocked-by or sub-issue dependency is removed upstream, so the item leaves Blocked. It removes only an edge its own `linked` created: an edge from an agent's prerequisite, from an answer that moves the work, or from a person's or an agent's link stays, and the tracker refuses ingest's `linked` and `unlinked` for it. An agent's prerequisite that names an edge ingest made first takes it over, and ingest does not retry a link refused as someone else's on every touch. A failed, rate-limited or partial listing unlinks nothing, and neither does a dependency that was transferred or is in a repo ingest cannot read.
- Fixed: A tracker with teams on renews the org writer lease as soon as it starts serving, instead of a third of a lease later, and takes it again at once when it ran out while the teams started. Until then a team create answered `503` with `org_unavailable` (#234). `GET /v1/org/teams` now answers `org_writable`, so a client can wait for the lease, and `dev/check-teams.mjs` does.
- Fixed: An ingest worker that lost its saved state no longer appends two context records for every open root. The tracker answers a `context` from ingest whose text matches what the entry already holds, on an entry no agent wrote, with `unchanged: true` and logs nothing (#235). Nothing in the fold changes, so old logs replay as before.
- Fixed: Redaction now cuts a key with no `END` whose first body line is short, OpenSSH 70-column and blank-line-led bodies, double-escaped and `\n\`-continued bodies, a digit-free Bearer in an escaped JSON header, stuck-together AKIA ids and JWTs, and `xoxd-` cookies. Records already on the log are unchanged. The edge cases left are listed in the threat model.
- Fixed: A tracker with several teams renews each team's writer lease as soon as the team starts serving, instead of a third of a lease later. A lease that lapsed (ran out without a renewal) while later teams started is renewed if Felix still has it, and taken again otherwise; after taking it again the team rebuilds its state from the log before writing. Before, the renewal of a lapsed lease was refused, the team stopped serving for good, and with one team the process exited. The org lease renews the same way, so a renewal just past its expiry no longer refuses org writes for a third of a lease. Two renewals at once no longer fence a team that holds its lease (#239).
- Fixed: Org and team writes are conditional on the offset they expect to land at, so a tracker that lost its lease while paused can no longer append after the new holder's records. After a refusal the tracker reads the log again and writes nothing more until Felix renews the lease by version after that read, which also holds after taking the lease at start or again once it lapsed; the next offset comes from the read, never from the refusal. An org write also gives up with 503 once the lease it started on runs short, instead of holding up the renewal (#242).
- Fixed: A lease put that landed in Felix with its reply lost no longer fences the tracker that holds the lease. On a refused or unanswered lease put, the tracker reads the lease entry and, when it names this tracker's random holder token, adopts its version and renews by it; with its view of the lease already run out it treats the entry as a retake and reads the log again before writing. This covers team and org renewals, retakes, and the lease taken at start (#244).
- Fixed: Redaction now cuts a private key whose line breaks were stripped, PGP `PRIVATE KEY BLOCK` keys, the `Proc-Type:`, `DEK-Info:` and armor lines before a key body (also in an added diff hunk), key bodies whose line breaks are written as `\u000a` or `%0A` (a URL-encoded body's `%2B`, `%2F` and `%3D` count as base64), secrets right after a URL-encoded byte such as `%20` or `%3D`, and two AWS access key ids joined by one byte. A mixed first body line longer than 71 bytes after a header with no `END` is now cut as a stripped body. Records already on the log are unchanged.
- Fixed: A write whose caller disconnects mid-flight (a client that hangs up) no longer lets its retry write a second record: the tracker rebuilds its state from the log before the next call. This also closes the same gap for retried claim-ending calls.
- Fixed: Redaction now cuts private keys whose header and `END` line are URL-encoded (`-----BEGIN%20PRIVATE%20KEY-----`, `-----BEGIN+PRIVATE+KEY-----`), including one pasted on one line with `%20` between its runs; key bodies in JSON that escape `/` as `\/` or write `+`, `/` and `=` as `+`, `/` and `=`; and PGP keys with more than 16 armor lines before the body. An armor line whose value starts another key header now ends the armor, and the redaction reads that header as a key of its own. Records already on the log are unchanged.
- Fixed: The redaction linearity test now compares one adversarial input at two sizes and checks the time grows about 4x for 4x the bytes. It no longer fails on a slow runner.
- Fixed: The two remaining redaction linearity tests now compare each input at two sizes instead of asserting a wall-clock bound, so they no longer fail on a slow runner.
- Fixed: Recording a merge (`merge_marked`) is refused while upstream shows the request open, and the item keeps who recorded it and at which offset (`merge_marked` on the item, shown as marked merged by that person). If upstream later shows that request open or closed unmerged, the item reopens into Needs you as `merge_disputed`; an upstream merge or a reopen clears the mark. A review ask made while upstream shows changes requested now lasts until the review state changes, not until the same state repeats, and a new attempt drops any ask and send-back. The wasm golden `format_11` changes: its state now carries `merge_marked` on the item a person marked merged.
- Fixed: Ingest finishes admitting an imported root after a poll fails partway through it, and reopens a root whose close was written but whose save was lost when the issue is open again by the next poll.
- Fixed: A tracker no longer hands an agent back a claim whose hold another writer already released, as after a broker failover. Before a resumed `work_next` or an `extend` it reads the log from the end of its state and rebuilds when anything landed there, so two agents cannot work one attempt.
- Fixed: The tracker prunes snapshot files by the log they belong to, so files left from before a retention trim are removed, and a save that started before a rebuild no longer writes or prunes after it.
- Fixed: A tracker restart keeps a direct claim (one handed straight to an item's agent assignee) even when the item was reassigned while the agent held it. Before, the restart polled the ready lanes for it, which spent a Felix handout on other items' head records, and then released it as lapsed before the deadline the agent was given, so a new assignee could start a second attempt while the old holder still worked. The tracker now records whether each claim was direct, keeps such a hold for one visibility period from the restart, and polls nothing for it. Snapshots from earlier builds are not read; the tracker folds the log instead.

  The dashboard's assign picker lists the team's agents beside people, from the new person route `GET /v1/work/agents` (agents not revoked), and assigns with `{"agent": name}`.
- Fixed: An Azure DevOps listing that hits its 2000-item cap inside one change instant now resumes after the last id read, so the cursor no longer stalls on the same items.
  A refused Azure DevOps state change is dropped only when the process names a rule or transition error; other failures are retried.
- Fixed: The GitLab source no longer skips an issue that is updated while its project is being read: each request restarts from the last time seen instead of paging by number. It reads times with a UTC offset (`+01:00`) instead of treating them as unknown, and a GitLab root now shows its repo in the work views.
- Fixed: Jira: a failed rule search stalls the project instead of dropping issues from scope, a `jql` with `ORDER BY` is refused, and the worker refuses to start when it cannot learn its own name.
- Fixed: A failed run log write is counted by the next write that lands, a record that is not a task is logged by its group offset when dead-lettered, and `task_claim_lost` says whether Felix refused the extension with `stale_claim` or none worked for a visibility window. The gap count is kept in memory, so it is reported only if that `kbl-mcp` process logs again.
- Documentation: A design for a dashboard Sources view: connect GitHub, Azure DevOps, Jira and GitLab sources with admin-consented credentials, prove a person may connect them, and pick up changes without restarting ingest. It also designs ingest for one org-wide installation serving many teams: a shared request budget with adaptive polling, a tracker-side webhook router whose deliveries are bound to one connection and deduplicated, per-source state with leases so several workers split a connection and stop upstream calls before a lease can move, and worker keys pinned on each worker that can be added without pasting secrets again (`docs/design/sources.md`).
- Documentation: Status badges in the README.

