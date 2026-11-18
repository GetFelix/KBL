# Threat model

Who can reach KBL (Kanban By Log, pronounced Kibble), what they could damage, and the check that stops each
attempt. Every tracker route, ingest route, MCP tool and record kind has a row.

A pull request that adds a route, a tool or a record kind adds its row here.
`scripts/check-threat-model.mjs` fails CI when one is missing. It finds routes
and tools in every crate of the workspace, and a path served with more than one
method needs a row per method, named like `` `PUT /v1/x` ``.

Each citation below names code as `file#symbol`: a function, constant or type,
written `Type::method` when the file declares the name more than once. Lines
change with every merge, so none are cited. `docs/threat-model-citations.json`
lists, per symbol, the exact lines the row relies on, and the check fails when
the symbol is gone or ambiguous, or no longer holds one of them. A new citation
fails until you add its entry there: pin the line that does the check, not a
closing brace near it.

A cell marked **None** is a gap with an open issue. The gaps are listed at
the end.

## Actors

| Actor | What it is | Credential | Trust |
|---|---|---|---|
| Person | Someone using the dashboard or the REST API | A person token from the tracker's token file | Trusted. Every person token can do everything a person can ([#120](https://github.com/GetFelix/KBL/issues/120)) |
| Agent | A model acting through `kbl-mcp` | An agent token for the tracker, and a Felix token for its run | Not trusted. Assume it follows any instruction it reads in an issue, a comment or a file |
| Ingest | The `kbl-ingest` worker that mirrors GitHub, GitLab, Azure DevOps or Jira | An ingest token, and a GitHub, GitLab, Azure DevOps or Jira credential | Trusted code that carries untrusted text |
| Gateway reader | A browser reading through felix-gateway | A gateway session scoped to one run | Read only (`dev/scope.toml`) |
| Webhook sender | Whoever posts to ingest's `/webhook` | The shared webhook secret | Not trusted without a valid signature |

Two more parties are outside KBL and can still steer it:

- **Anyone who can file or edit an issue or comment** in a repo an intake rule
  admits. Their text reaches agents as context.
- **Anyone who can push to an agent's branch or administer the repo.** They can
  make checks fail or add approvals. This is GitHub's own trust; the tracker
  never merges.

## Assets

| Asset | Where it lives | What an attacker wants |
|---|---|---|
| The log | `work.events` in Felix | Records that did not happen, or records under another name |
| Budgets | Folded from the log, per root | Free work, or more items, depth and redos than allowed |
| Holds | Folded from the log, plus the tracker's delivery table | To write on work it does not hold, or to keep work from others |
| Merges | GitHub. KBL records them and never merges | An item closed as merged that was not, or a merge into the wrong branch |
| Rules | `rules_set` records, shown in every claim reply | Instructions injected into every other agent |
| Artifacts | The artifacts cache, keyed by content hash | Other bytes shown under a trusted id |
| Credentials | The token file, the webhook secret, the GitHub credential (a token, or a GitHub App's private key, which ingest trades for short-lived installation tokens), the Jira credential (an API token or an OAuth access token), the Azure DevOps credential (a personal access token, or an Entra service principal's client secret and the access token it yields), the GitLab credential (a group or project access token), Felix tokens | Any of them |

## Trust boundaries

1. **Agent to tracker.** The bearer token names the caller
   (`tracker/src/http.rs#from_request_parts`). The name and role on a record come from the
   token, never from the body (`tracker/src/tracker.rs#finish`). The attempt
   number comes from the hold, never from the body
   (`tracker/src/tracker/tools.rs#held`, `tracker/src/tracker.rs#finish`).
2. **Tracker to log.** The tracker is the only writer of `work.events`. It
   appends a record only if the fold accepts it
   (`tracker/src/tracker.rs#append_expecting`), so every rule in
   `core/src/work_fold.rs` is a check on every route. A writer lease keeps a
   second tracker from writing (`tracker/src/tracker.rs#append_expecting`), and every
   append is conditional on the offset it expects, so a tracker that lost the
   lease cannot append after the new holder's records. After a refusal, or a
   write whose answer was lost, it reads the log again and writes again only
   once Felix grants it the lease after that read. Residual: a stale holder's
   write still lands if the new holder has written nothing since. The
   new holder's next write is then refused and it reads the stale record
   before writing, so the record is not lost, but it takes effect without the
   lease: a revoke or release written that way counts, late.
3. **Ingest to tracker.** A separate token and a short list of record kinds,
   only on roots ingest imported (`tracker/src/tracker/ingest.rs#allowed`,
   `tracker/src/tracker/ingest.rs#ingest_record_in`).
4. **GitHub to ingest.** Issue text is data. The pull request an item reports
   is named by an agent, so ingest ties it to the item before it reads it
   (see [What ingest believes](#what-ingest-believes)).
5. **Agent to Felix.** `kbl-mcp` runs beside the model with the agent's
   Felix token. Nothing in `kbl-mcp` is a security check: an agent with its
   token and a raw client skips it. Felix grants are the check.

`kbl-mcp` takes its Felix token from `KBL_FELIX_TOKEN`, or renews one from an
ID token it reads from a file or a command (`mcp/src/credential.rs`). Both come
from the environment or a file, never a flag, so nothing shows in the process
list. The ID token file must be mode 600 or 400 (`mcp/src/credential.rs#read_file`).
The ID token is sent only to an `https` control plane, or `http` on this
machine (`mcp/src/credential.rs#exchange_url`), and the request does not
follow redirects (`mcp/src/credential.rs#http_client`). Errors name the step and the status, never a
token, the control plane's reply or the command line, and the command's stderr
is dropped. The ID token is the standing secret here: whoever reads the file or
runs the command can mint Felix tokens until the ID token expires or is revoked,
so give an agent one narrowed to its own run, not the administrator's. A
failed renewal keeps the current Felix token until it expires. Revoking the ID token stops renewals, not the Felix token in hand, and a stream already open outlives its
token's expiry (Felix `docs/auth.md`), so cut an agent off by revoking its Felix principal, not only the ID token.
6. **Browser to gateway.** Read only, one run.

When ingest signs in as a GitHub App, the private key is the standing secret:
anyone who reads the key file can mint installation tokens until the key is
revoked in GitHub. Keep it mode 600 or 400; ingest refuses to start with a looser key file. The key, the
signed JWT and the installation token are held in memory, never logged, and
left out of errors (GitHub's error text goes through the redactor first). An
installation token lives an hour and is replaced five minutes early, so a
leaked one is short-lived; the key is not.

When ingest signs in to Jira, the API token or OAuth access token is read from
an environment variable or a file for each request (`ingest/src/jira/transport.rs#Secret`) and
sent only as that request's `Authorization` header. It is not in the config,
the saved state, any record or any log line. Jira text is untrusted like
GitHub's: a filter that matches an issue says only that the issue is in scope,
never that its text is safe. Ingest writes to Jira only what its rule asks for
(a comment, a remote link, a workflow transition), only on an issue it
imported, and only for a pull request in a repository the rule maps issues to
(`ingest/src/sync.rs#announce`). That pull request is the agent's claim: a Jira
worker does not read pull requests, so only the repository is checked, not that
the pull request exists or is ours, and the link and comment on the issue say
what the agent named. An issue whose component maps to no repository
is imported but stays in Needs you (`ingest/src/sync.rs#park_unmapped`), so no
agent is handed work with no repository to run in.

When ingest signs in to Azure DevOps, the personal access token or the Entra
client secret is read from an environment variable or a file each time it is
used (`ingest/src/ado/transport.rs#Secret`) and sent only as that request's
`Authorization` header, or to the Entra token endpoint. The access token Entra
returns is held in memory until two minutes before it expires. None of them is
in the config, the saved state, any record, any error or any log line: an
Entra refusal reports only its status and error code, not the provider's
description (`ingest/src/ado/transport.rs#token`). New Azure DevOps OAuth app
registrations are deprecated, so there is no OAuth path. The credential needs
Work Items (Read), and Read & write only when a rule writes back; the adapter
refuses every write when no rule asks for one, so a worker given a read-only
credential cannot be steered into writing (`ingest/src/ado.rs#write_back`). A
service principal has no scopes, so its rights are whatever the organization
granted it, which the operator should keep to the projects the rules read.
Writes go only to a work item ingest imported (the engine writes back only for
its imported roots, `ingest/src/sync.rs#announce`) and only in a project a rule
reads (`ingest/src/ado.rs#project_of`). Work item text is untrusted like
GitHub's and is never put into a query or an address: queries are built from the
configured project, a date that is checked, and ids; a saved query is named by the
configuration and its path may not climb out of the queries tree
(`ingest/src/ado.rs#wiql_date`, `ingest/src/config.rs#check_ado`). Text written to a
work item is HTML-escaped (`ingest/src/ado.rs#html_escape`). A work item whose
area path maps to no repository is imported but stays in Needs you
(`ingest/src/sync.rs#park_unmapped`). A bad personal access token is answered
203 with a sign-in page, not a 401; the adapter reads it, and a redirect, as a
refusal so the project stalls and nothing is imported from the page
(`ingest/src/ado.rs#call`).

When ingest signs in to GitLab, the group or project access token (a bot user)
is read from an environment variable or a file for each request
(`ingest/src/ado/transport.rs#Secret`) and sent only as that request's
`PRIVATE-TOKEN` header, marked sensitive
(`ingest/src/gitlab/transport.rs#send`). It is not in the config, the saved
state, any record, any error or any log line: an error carries a status and
never GitLab's answer (`ingest/src/gitlab.rs#call`). Redirects are never
followed, so a token cannot be carried to another host, and one is read as a
refusal. The instance address comes from the config alone and must be https,
or http on loopback; an address with a user name, password, query or fragment
is refused at startup (`ingest/src/gitlab.rs#base_url`,
`ingest/src/config.rs#check_gitlab`). A page of a listing is
asked for by number, never by following a link the server sends, and no
address or query is built from issue text, so an issue cannot steer a request
(SSRF through the configured address is the operator's choice, and through
item text is not possible). The token needs `read_api`, and `api` only when a
rule writes back; the adapter refuses every write, including drafting a merge
request, when no rule asks for one
(`ingest/src/gitlab.rs#write_back`). Writes go only to a project a rule reads
(`ingest/src/gitlab.rs#project_of`), only for an issue ingest imported
(`ingest/src/sync.rs#announce`), and only as a comment, a `close` or `reopen`,
or a draft title on a merge request our own login opened. GitLab sends no
word of who merged beyond the merge request: a merge counts only into the
project's default branch, and only when GitLab names a merger who is not one
of our logins, so a merge whose merger or default branch cannot be read counts
nothing (`ingest/src/gitlab.rs#pull_request`). A system note and an internal
note are not imported as comments (`ingest/src/gitlab.rs#parse_note`).

`kbl-ingest check` writes nothing upstream. Its connection to the source refuses
every request that is not a GET (`ingest/src/check.rs#ReadOnly::send`). Azure DevOps
reads work items with a query that is a POST, so its guard lets through one
request and no other: a POST to `.../_apis/wit/wiql` whose body is a lone `SELECT`
query, which the service cannot turn into a write
(`ingest/src/check.rs#ReadOnlyWiql::send`, `ingest/src/check.rs#is_wiql_query`). Error text from a failed probe is run
through the secret redactor and never carries a response body
(`ingest/src/check.rs#safe`).

The token file must be mode 600 or 400 (`tracker/src/config.rs#load`), a token
may be listed once (`tracker/src/config.rs#parse`), and the lookup takes the
same time for a wrong token as for a right one (`tracker/src/config.rs#entries`).
The tracker and the webhook serve plain HTTP, so each refuses to listen on
an address other than loopback unless a flag says a TLS proxy is in front
(`tracker/src/main.rs#check_listen`, `ingest/src/main.rs#check_listen`). The flag is a promise
the process cannot check: set it without a proxy and tokens cross the network
in the clear. Callers are not checked either: `kbl-mcp` and ingest send their
token to whatever tracker URL they are given, `http://` included. Tokens are long-lived ([#121](https://github.com/GetFelix/KBL/issues/121)).
There is no request rate limit; a refused call writes nothing, and the first
cap an attempt hits is logged once (`tracker/src/tracker/tools.rs#note_refusal`).

The snapshot folder (`KBL_TRACKER_SNAPSHOT_DIR`, off by default) holds a copy of
each team's folded state, so whoever can write to it can change what the tracker
believes at the next start. It is created mode 700 and is trusted like the token
file. A file is used only if a digest over its body matches, its fold revision is
the build's, and a hash over every record it covers still matches `work.events`
(`tracker/src/tracker/snapshots.rs#restore`); the hash does not cover the state
inside the file, so the digest stops damage and not a deliberate edit. A file that
fails is removed and the next one is tried, then a full fold. A file is written
to a temp file, synced, renamed and the folder synced
(`tracker/src/tracker/snapshots.rs#write`), so a crash leaves the old one.

## Tracker routes

Every route but `/healthz` needs a bearer token. A wrong kind of token gets
403. Request bodies are limited to 2 MB by the HTTP layer.

### Request keys

Every agent and person write route but the calls that end a claim takes an
optional `request_key` ([the API](work-tracker.md#request-keys)). It is
chosen by the caller, so the caller controls the key and nothing else.

| Attack | Check |
|---|---|
| Use a key to read what another caller wrote, or to stop their write | Keys are kept per caller and kind of caller, so another caller's key finds nothing (`tracker/src/tracker/request_keys.rs#keyed`) |
| Reuse a key with another body to slip a different write past a check, or get the first write's answer for it | The digest of the route and body must match, or the call is `409` `request_key_reused` and writes nothing (`tracker/src/tracker/request_keys.rs#keyed`) |
| Replay a write the agent may no longer make, such as after losing its claim | A replay writes nothing and answers only with the caller's own first record: its kind, item and offset. The token is checked first |
| Fill the tracker's memory with keys | The last 256 keys of each caller are kept (`tracker/src/tracker/request_keys.rs#remember`); a key is also bounded as a name |
| Send the same key twice at once to write twice | A key whose write is running is refused (`tracker/src/tracker/request_keys.rs#take`) |

### Agent routes

All of them need an agent token (`tracker/src/http.rs#agent`).

| Route | Fields the agent controls | What they could steer | Check |
|---|---|---|---|
| `/v1/work/next` | `wait_seconds`, `review_tiers`, `kinds`, `tiers`, `repo`, `exclude_roots` | Hold a connection open; claim work it should not | Wait capped at 30 s (`tracker/src/tracker.rs#claim_loop`). A claim needs a Ready item, tries left and cost left (`core/src/work_fold.rs#step_record`). Any agent may claim any Ready item ([#120](https://github.com/GetFelix/KBL/issues/120)) except one assigned to another agent: the fold refuses that claim as `not_assignee` (`core/src/work_fold.rs#step_record`), and the tracker never publishes such an item to a lane, hands it only to its assignee (`tracker/src/tracker.rs#try_assigned`), and acks a ready record of it that another agent polls, without a nack or a new publish (`tracker/src/tracker.rs#claim_from`), so no agent loops on it or spends Felix's handouts. An agent assignee that is revoked or calls nothing for `KBL_ASSIGNEE_AWAY_SECS` is dropped by the sweep (`tracker/src/tracker.rs#drop_away_assignees`). `review_tiers`: a review is given only to an agent that names its tier; at most 4 names, an unconfigured one is refused (`tracker/src/tracker.rs#review_lanes`). An agent that worked a tree is skipped for its reviews (`tracker/src/tracker.rs#try_claim`, `tracker/src/tracker.rs#try_claim`) and the fold refuses the claim too (`core/src/work_fold.rs#step_record`). A held claim is handed back, or extended, only while the log's tip still shows the hold (`tracker/src/tracker.rs#hold_at_tip`). Filters only narrow: a filtered or turn-taking claim is chosen from the same candidates a plain claim may take (`tracker/src/tracker/selection.rs#candidates`), skips another agent's assignee items, reviews of the agent's own work and unnamed review lanes, and goes through the same fold and tail check (`tracker/src/tracker.rs#direct_claim`); no ready record is polled for an item that was passed over. Unknown kinds or tiers and more than 64 excluded roots are refused (`tracker/src/tracker/selection.rs#parse`) |
| `/v1/work/peek` | `limit` (clamped to 50), `review_tiers`, `kinds`, `tiers`, `repo`, `exclude_roots` | Learn more than the caller may claim; use it to wear the tracker down | Agent token only. It lists items the caller could claim and nothing else, from the state in memory: no write, no ready record polled (`tracker/src/tracker/selection.rs#peek`). An item assigned to another agent is not listed |
| `/v1/work/extend` | `item` | Keep another agent's claim alive, or its own after losing it | Must own the delivery and the hold (`tracker/src/tracker.rs#holding`, `tracker/src/tracker.rs#holding`) |
| `/v1/work/complete` | `item`, `claim`, `attempt`, `usage` | Finish work it does not hold; finish over open children; report no usage, or a model with no price; claim a replay of an outcome it did not write | `claim` (or `attempt`) only replays a record this agent wrote, of the same kind, ending that claim (by attempt, its own latest claim of it); a lapse the tracker wrote stays `lost`, and a `claim` or `attempt` that is not the one held ends nothing (`tracker/src/tracker.rs#ended_by`, `tracker/src/tracker.rs#finish`). Holder only (`tracker/src/tracker.rs#finish`, `core/src/work_fold.rs#step_record`). Open children refuse it (`core/src/work_fold.rs#step_record`). A fix is not closed by `done`; a merge or a person closes it. Usage: what each model used is the agent's word; the fold prices it and charges at least the floor the tracker writes on the record, `KBL_COST_FLOOR_MICROS` (`core/src/work_fold.rs#charge`). At most 16 entries, one per model, plain model names; a `cost_micros` is refused (`core/src/work_fold.rs#check_charge`, `tracker/src/http.rs#usage_of`); the item total saturates (`core/src/work_fold.rs#charge`) |
| `/v1/work/release` | `item`, `claim`, `attempt`, `reason`, `why`, `usage` | Release another's work; write a huge record; dodge the two tries by calling a failure a wait; take back an item another agent claimed after a lapse | `claim` and `attempt` replay only as for `complete`. Holder only (`core/src/work_fold.rs#step_record`), except that from format 14 the agent whose hold lapsed may release as a wait once, while nobody has claimed since (`core/src/work_fold.rs#late_writer`, `tracker/src/tracker.rs#lapsed_attempt`); a claim clears that window (`core/src/work_fold.rs#step_record`). A wait gives the claim back (`core/src/work_fold.rs#park_wait`) but flags the item into Needs you, so only a person sends it out again; a late failure is refused (`tracker/src/tracker.rs#release`). Two claims per attempt (`core/src/work_fold.rs#step_record`). `reason` is held to 4 KiB (`tracker/src/tracker/tools.rs#check_sizes`). Usage: what each model used is the agent's word; the fold prices it and charges at least the floor the tracker writes on the record, `KBL_COST_FLOOR_MICROS` (`core/src/work_fold.rs#charge`). At most 16 entries, one per model, plain model names; a `cost_micros` is refused (`core/src/work_fold.rs#check_charge`, `tracker/src/http.rs#usage_of`) |
| `/v1/work/give_up` | `item`, `claim`, `attempt`, `reason`, `usage` | Same as release | `claim` and `attempt` replay only as for `complete`. Holder only (`core/src/work_fold.rs#step_record`). `reason` is held to 4 KiB (`tracker/src/tracker/tools.rs#check_sizes`). Usage: what each model used is the agent's word; the fold prices it and charges at least the floor the tracker writes on the record, `KBL_COST_FLOOR_MICROS` (`core/src/work_fold.rs#charge`). At most 16 entries, one per model, plain model names; a `cost_micros` is refused (`core/src/work_fold.rs#check_charge`, `tracker/src/http.rs#usage_of`) |
| `/v1/work/post` (child or proposal) | `item`, `key`, `work`, `title` | Create work outside its tree, flood Triage with proposals, or create a kind it may not | Must hold `item` (`tracker/src/tracker/tools.rs#post`). Only split, augment, question, test or a proposal (`tracker/src/tracker/tools.rs#parse`). Children draw from the root's items and depth (`core/src/work_fold.rs#propose`). 24 posts per attempt (`core/src/work_fold.rs#propose`). A proposal waits for a person; an agent may have 16 waiting at once and 8 from one root, over all its attempts, and a decision frees a slot. The tracker checks this before it appends (`proposal_room`, `tracker/src/tracker/tools.rs`), so the fold of an old log does not change. Proposals draw no budget and a key can be reused across claims ([#109](https://github.com/GetFelix/KBL/issues/109)) |
| `/v1/work/post` (`work: "prerequisite"`) | `target_source`, `target_repo`, `title`, `why`, `evidence`, `existing`, `done_when`, `claim`, `attempt`, `usage` | File work in another repo's Triage, without limit; link to another tree | `claim` and `attempt` replay only as for `complete` (`tracker/src/tracker/tools.rs#prerequisite`). Only a source and repo in the agent's grants or those of its own tree; a grant names both (`tracker/src/config.rs`), and a missing item answers the same as a hidden one (`tracker/src/tracker/tools.rs#prerequisite`). Holder, no cycle (`core/src/work_fold.rs#prerequisite`), not closed (`core/src/work_fold.rs#prerequisite`). From format 5, `done_when: released` may name an item closed as merged or completed with no release yet (`core/src/work_fold.rs#prerequisite`). A new one counts against the same pending-proposal caps as `proposal`. Sizes (`tracker/src/tracker/tools.rs#prerequisite`). Evidence target: a `work.events` record of the same tree, or the record its context entry says made the item; another stream is refused (`tracker/src/tracker.rs#evidence_in_tree`). Usage: what each model used is the agent's word; the fold prices it and charges at least the floor the tracker writes on the record, `KBL_COST_FLOOR_MICROS` (`core/src/work_fold.rs#charge`). At most 16 entries, one per model, plain model names; a `cost_micros` is refused (`core/src/work_fold.rs#check_charge`, `tracker/src/http.rs#usage_of`). `title` and `why` are read by a person before accepting. From a held triage item the same checks apply, except that nothing is charged and `usage` sent is not recorded (triage reports it when it completes), and the item under triage waits instead (`tracker/src/tracker/tools.rs#prerequisite`): the agent can hold up the item it triages, as it can with `unclear`, until a person removes the link |
| `/v1/work/link` | `item`, `to` | Link across trees; block work it does not hold | Same tree only (`core/src/work_fold.rs#agent_may_link`). Must hold `item`, a child of it or its parent (`core/src/work_fold.rs#agent_may_link`). No cycle (`core/src/work_fold.rs#step_record`). 100 links per attempt (`core/src/work_fold.rs#step_record`). A repeat is refused (`tracker/src/tracker/tools.rs#link`) |
| `/v1/work/context` | `item` | Read context of work it does not hold | Must hold the item or its parent (`tracker/src/tracker/tools.rs#context_read`) |
| `/v1/work/context/add` | `item`, `entry`, `text` | Write on work it does not hold; replace what ingest or a person wrote | Must hold the item or a child of it (`core/src/work_fold.rs#step_record`). Text inline up to 4 KiB (`tracker/src/tracker/tools.rs#check_sizes`). 100 writes per attempt (`core/src/work_fold.rs#step_record`). An agent replaces only an entry it wrote itself; ingest's, a person's and the fold's stay, refused as `not_your_entry` before the record is logged (`tracker/src/tracker/tools.rs#context_add`). From format 6 the `prerequisite` entry is refused to agents even where none exists. The fold takes such a record from an old log as it always did. A `files` entry puts an overlap warning, and a suggestion that one item wait on another, on the other items' cards in the same repo (`core/src/overlap.rs`). It is only a suggestion: nothing links or blocks until a person records a `linked`. The record names the agent in `by`, and the fold keeps it per entry in `context_agents` |
| `/v1/work/progress` | `note`, `branch`, `pr` | Point the item at someone else's pull request so its merge or checks count | Holder only (`core/src/work_fold.rs#step_record`). One request, one open item (`core/src/work_fold.rs#step_record`). Sizes (`tracker/src/tracker/tools.rs#check_sizes`). 100 per attempt (`core/src/work_fold.rs#step_record`). `pr` is only a name: see [What ingest believes](#what-ingest-believes) |
| `/v1/work/decide` | `phase`, `so_far`, `question`, `options`, `chosen`, `why`, `commit`, `evidence`, `revert_check` | Fill the log; cite evidence that is not there; claim a test guards a fix | Holder only (`core/src/work_fold.rs#step_record`). Sizes and counts (`tracker/src/tracker/tools.rs#progress`, `tracker/src/tracker/tools.rs#decide`). 200 per attempt (`core/src/work_fold.rs#step_record`). Evidence is before the record (`core/src/work_fold.rs#step_record`). Test name 400 bytes (`core/src/work_fold.rs#step_record`). A revert check is the agent's word; only one that failed without and passed with the fix is listed as proven. Evidence target: a `work.events` record of the same tree, or the record its context entry says made the item; another stream is refused (`tracker/src/tracker.rs#evidence_in_tree`) |
| `/v1/work/finding` | `item`, `severity`, `file`, `line`, `title`, `scenario`, `verdict` | Fill the log; make fix work without limit; write on a review it does not hold or for an older request | Holder of a review item only (`core/src/work_fold.rs#finding`, `core/src/work_fold.rs#review_of`). Sizes: file 400, title 200, scenario 4096 bytes, line from 1 (`tracker/src/tracker/tools.rs#check_sizes`, `core/src/work_fold.rs#finding`). 40 per attempt (`core/src/work_fold.rs#finding`). The request must still be the root's (`core/src/work_fold.rs#finding`). A `fix` draws one item from the root's budget; a `defer` is a proposal a person accepts and counts toward the same pending-proposal caps as `proposal` |
| `/v1/work/verdict` | `item`, `verdict`, `why` | Recommend a merge with fixes open; give a verdict from a first pass; skip the verdict | Holder of a final review only (`core/src/work_fold.rs#step_record`, `core/src/work_fold.rs#step_record`). `why` at most 2048 bytes (`tracker/src/tracker/tools.rs#check_sizes`). `merge` is refused while a fix a review asked for is open (`core/src/work_fold.rs#step_record`). A final review cannot finish without one (`core/src/work_fold.rs#step_record`). It is advice: nothing here merges or closes |
| `/v1/work/triage` | `item`, `tier`, `why`, `duplicate_of`, `unclear`, `depends_on`, `related`, `split` | Triage an item it holds no triage for; raise its own work's tier; park an item in Needs you; point a duplicate at nothing; fill the log | Holder of the triage item only, and the record names that item's parent, never an item the agent picks (`tracker/src/tracker/tools.rs#triage`, `core/src/work_fold.rs#may_triage`). It is a proposal: a tier a person set stands (`core/src/work_fold.rs#step_record`). `why` at most 4096 bytes (`tracker/src/tracker/tools.rs#triage`). `duplicate_of` must be another item the agent can see; a missing item and one outside its grants get the same refusal (`tracker/src/tracker/tools.rs#triage`). `depends_on` follows the same rule, and may not name the item itself or repeat; `related` follows it too and may not also be in `depends_on`, and naming one is refused as `needs_format_18` below format 18 (`tracker/src/tracker/tools.rs#triage`); `depends_on`, `related` and `split` hold at most 8 entries and a split title at most 256 bytes (`triage_lists`, `tracker/src/tracker/tools.rs#triage_lists`). None links, relates or creates anything: a person acts on them. A tier never changes the tier an admitted root has. `unclear` only sends the item to a person as a question. The same answer again from the same holder is not logged; an attempt logs at most 4 different answers, then the call is refused; a new holder of the same attempt shares the count (`triage_full`, `core/src/work_fold.rs`) |
| `/v1/work/step` | `command`, `exit_code`, `output` | Claim a command passed | Holder only (`core/src/work_fold.rs#step_record`). Sizes (`tracker/src/tracker/tools.rs#check_sizes`). 500 per attempt (`core/src/work_fold.rs#step_record`). A step is the agent's word. Check results come from ingest, never from steps |
| `/v1/work/edit` | `file`, `hunk`, `decision` | Tie an edit to another attempt's decision | Holder only (`core/src/work_fold.rs#step_record`). The decision is one of this attempt's (`core/src/work_fold.rs#step_record`). Sizes (`tracker/src/tracker/tools.rs#check_sizes`). 500 per attempt (`core/src/work_fold.rs#step_record`) |
| `/v1/work/ask` | `text`, `options`, `default`, `usage` | Flood Needs you; report no cost; loop an item without a person by asking | Holder only, or from format 14 the agent whose hold lapsed, once (`core/src/work_fold.rs#step_record`, `core/src/work_fold.rs#late_writer`). From 14 the ask ends the hold and gives the claim back (`core/src/work_fold.rs#park_wait`); the item stays in Needs you until a person answers. At most 8 options of 200 bytes (`tracker/src/tracker/tools.rs#check_options`). 20 per attempt (`core/src/work_fold.rs#step_record`). Usage: what each model used is the agent's word; the fold prices it and charges at least the floor the tracker writes on the record, `KBL_COST_FLOOR_MICROS` (`core/src/work_fold.rs#charge`). At most 16 entries, one per model, plain model names; a `cost_micros` is refused (`core/src/work_fold.rs#check_charge`, `tracker/src/http.rs#usage_of`); the item total saturates (`core/src/work_fold.rs#charge`) |
| `/v1/work/answer` | `item`, `question`, `text`, `chosen`, `evidence`, `moves_to` | Answer a question nobody asked it; move work to another repo | `moves_to` is refused (`tracker/src/http.rs#answer`) and the fold refuses it from an agent (`core/src/work_fold.rs#check_then`). Must hold a question item under `item` (`core/src/work_fold.rs#may_answer`). Only the open question (`core/src/work_fold.rs#step_record`). `chosen` is one of the options (`core/src/work_fold.rs#step_record`). 10 per attempt (`core/src/work_fold.rs#step_record`). The resume item obeys the depth limit (`core/src/work_fold.rs#check_then`). Evidence target: a `work.events` record of the same tree, or the record its context entry says made the item; another stream is refused (`tracker/src/tracker.rs#evidence_in_tree`) |
| `/v1/work/rules/read` | `item` | Read rules of work it does not hold | Holder only (`tracker/src/tracker/tools.rs#rules_read`) |

### Person routes

All of them need a person token (`tracker/src/http.rs#person`). An agent or
ingest token gets 403. The fold then checks the record, and sizes are checked
in `check_sizes` (`tracker/src/tracker/tools.rs#check_sizes`).

| Route | Record | Check beyond the token |
|---|---|---|
| `/v1/work/records` | Any kind a person may write. A kind the tracker does not know is refused | The fold refuses the kinds a person may not write: see [Records on work.events](#records-on-workevents). The strings are held to 4 KiB when the record is appended, including progress `branch` and `pr`, `triaged.why`, `imported` fields and a rules target (`tracker/src/tracker.rs#sync_ready`) |
| `/v1/work/proposals/accept` | `accepted` | Tier 0 to 3 (`core/src/work_fold.rs#step_record`) |
| `/v1/work/proposals/edit` | `edited` | Only before acceptance (`core/src/work_fold.rs#step_record`). `depends_on` and `split` need format 17 and the `proposal` offset of the standing answer, so an edit cannot land on an answer triage replaced; dependencies must be distinct existing items other than the item and its triage item (`core/src/work_fold.rs#step_record`). Lists are bounded as for triage (`tracker/src/tracker/tools.rs#triage_lists`) |
| `/v1/work/proposals/reject` | `rejected` | Only a proposal never accepted (`core/src/work_fold.rs#step_record`) |
| `/v1/work/questions/answer` | `answer` | Only the open question; a `moves_to` target and title are bounded (`core/src/work_fold.rs#check_then`). Evidence as for an agent's answer. Known secret shapes redacted from the text, choice and title (`tracker/src/tracker/tools.rs#person_answer`) |
| `/v1/work/tier` | `tier_changed` | Tier 0 to 3, item open (`core/src/work_fold.rs#step_record`) |
| `/v1/work/note` | `human_input` | Item exists; an `attempt` must be one of the item's (`core/src/work_fold.rs#step_record`). Text inline up to 4 KiB (`tracker/src/tracker/tools.rs#check_sizes`). Known secret shapes redacted (`tracker/src/http.rs#note`) |
| `/v1/work/close` | `closed` | Item exists (`core/src/work_fold.rs#step_record`) |
| `/v1/work/unlink` | `unlinked` | Item exists (`core/src/work_fold.rs#step_record`) |
| `/v1/work/relate`, `/v1/work/unrelate` | `related`, `unrelated` | Person only in the fold too, both items exist (`core/src/work_fold.rs#step_record`). Format 18 |
| `/v1/work/budget` | `budget_set` | Person only in the fold too (`core/src/work_fold.rs#step_record`) |
| `/v1/work/prices` | `price_set` | Person only in the fold too, plain model name, and never effective before it is written, so a price cannot reprice usage already charged (`core/src/work_fold.rs#step_record`). `as_of` must be a real `YYYY-MM-DD` or the price is refused as `bad_price`; it only labels the price. An `expected_price` (a price, or `{"none": true}` for no price) that does not match what is in effect for the model and unit when the new price starts is refused as `price_changed` before the append, under the write lock, so a stale save cannot overwrite a price it never saw (`tracker/src/tracker.rs`, `price_is_expected`). Without it nothing is checked, so a client that omits it can still overwrite. Format 8, or 9 with `as_of`, so refused until a person raises the log |
| `/v1/work/rules` | `rules_set` | Person only in the fold too (`core/src/work_fold.rs#step_record`). 8 KiB of text, 20 checks of 500 bytes, 500 scopes (`core/src/work_fold.rs#step_record`). The tracker never runs a check; the agent does |
| `/v1/work/flag`, `/v1/work/unflag` | `flagged` | Person only in the fold too, item open and accepted, and the flag must change (`core/src/work_fold.rs#step_record`). Format 7, so refused until a person raises the log. A held item stops only once its holder finishes or releases (`core/src/work_fold.rs#needs_you`) |
| `/v1/work/redo` `{item, from_attempt, at, note?}` | `forked_from` | Person only in the fold too. The item must be open and not held, with no redo waiting; the attempt and the decision must exist on it; at most two attempts stay open; the root's `redos_per_item` bounds the count, so a person cannot start unlimited work. A red check on the open pull request needs a note newer than the failure (`tracker/src/tracker.rs#retry_has_note`). The route adds nothing `/v1/work/records` did not already accept from a person |
| `/v1/work/retry` | `retried` | Person only in the fold too, item open, held by nobody, and its open attempt used its tries and is not done; anything else is `no_change`, so a call cannot reset a count that is not stopping the item (`core/src/work_fold.rs#step_record`). Format 10 |
| `/v1/work/snooze` `{item, until_ms}` | `snoozed` | Person only in the fold too, item open and waiting on a person. `until_ms` must be after the record and at most 30 days past it, so one call cannot hide an item for good (`core/src/work_fold.rs#step_record`). It hides the item from My queue only, for the wait it was set on, and is not an action on the wait. Format 10 |
| `/v1/work/block` `{item, reason, until_ms?, resource?}`, `/v1/work/lift-block` | `block_set`, `block_cleared` | Person only in the fold too, item open. A block needs a time, a resource or both; the time must be after the record and at most 366 days past it, the resource 1 to 64 plain characters, the reason non-empty and at most 300 bytes, so one call cannot hold an item for good by accident (`core/src/work_fold.rs#step_record`). A repeat and a lift of nothing are `no_change`. Format 16, so refused until a person raises the log (`core/src/work_fold.rs#format_gate`). A claim on an item whose block holds is refused as `blocked`, judged at the claim's own time (`core/src/work_fold.rs#step_record`), and the tracker skips such an item before it asks (`tracker/src/tracker.rs#direct_claim`) |
| `/v1/work/classify` `{item, item_type?, area?, size?}`, a `vocabulary` on `/v1/work/rules`, `item_type`, `area` and `size` on `/v1/work/triage` | `edited`, `rules_set`, `triaged` | A type or area outside the team's vocabulary is refused as `bad_class` and the whole answer lands nothing; values are 1 to 40 plain characters, a list holds 32 distinct values, and a team with no list accepts no type or area (`core/src/work_fold.rs#step_record`). A person's value is never replaced by triage or an intake rule, and upstream's labels and milestone are written by the tracker's ingest only, never mixed with a person's edit (`core/src/work_fold.rs#step_record`). Format 19, so refused until a person raises the log (`core/src/work_fold.rs#format_gate`). A filter on them only narrows what an agent may take and is read from the root (`tracker/src/tracker/selection.rs`) |
| `needs` on `/v1/work/triage` and `/v1/work/classify`, `needs` in a `vocabulary` on `/v1/work/rules`, a `capabilities` list on `/v1/work/next` and `/v1/work/peek` | `triaged`, `edited`, `rules_set` | Each need must be listed in the team's vocabulary or the whole record is `bad_class`; a set holds at most 8 plain values of at most 40 characters, and triage and the tracker cannot replace a person's set (`core/src/work_fold.rs#step_record`). Format 20, so refused until a person raises the log (`core/src/work_fold.rs#format_gate`). Capabilities are read per call and never stored: at most 32 plain words, matched without regard to case. An agent is handed an item only when its capabilities cover the root's needs for a fix, test or resume (`core/src/work_fold.rs#can_take`), on every path that claims from the state (`tracker/src/tracker/selection.rs#Filter::matches`), and a claim from a polled record whose item gained a need after the candidates were read is acked instead (`tracker/src/tracker.rs#claim_from`). A call that cannot take a ready item claims from the state, so its record is not polled (`tracker/src/tracker/selection.rs#skips_for_needs`) |
| `/v1/work/lift-resource` `{resource}` | `resource_lifted` | Person only in the fold too. It ends only blocks that still hold on an open item and name exactly that resource; a block on another resource, a time-only block, an ended block and a closed item are left as they were, and a call that lifts nothing is `no_change` (`core/src/work_fold.rs#step_record`). The name is 1 to 64 characters (`bad_block` otherwise). Format 20 (`core/src/work_fold.rs#format_gate`) |
| `/v1/work/park`, `/v1/work/unpark` | `parked` | Person only in the fold too, item open, and parking needs an item that waits on a person; a repeat is `no_change` (`core/src/work_fold.rs#step_record`). Hides the item from My queue until it waits for a new reason, which shows it again (`core/src/work_fold/human_wait.rs#hide_covers`). It does not change the item's stage or what an agent may claim. Format 10 |
| `/v1/work/unblock` `{item, to}` | `unblocked` | Person only in the fold too, item open, and `to` must be a dependency that still holds the item; a link that holds nothing is `no_change` (`core/src/work_fold.rs#step_record`). Drops the same edge `unlink` drops, so the item can be claimed once nothing else holds it. Format 10 |
| `/v1/work/request-review` `{item, reviewer}` | `review_requested` | Person only in the fold too, item open, `reviewer` 1 to 100 bytes with no control character, and the item's request open upstream and not approved; a repeat of the same ask is `no_change` (`core/src/work_fold.rs#step_record`). It writes a note on the item and never touches `pr_status`; it is dropped when upstream answers or the request changes (`core/src/work_fold.rs#step_record`). Format 11 |
| `/v1/work/request-changes` | `changes_requested` | Person only in the fold too, same conditions as the ask, and a repeat is `no_change` (`core/src/work_fold.rs#step_record`). Starts no redo and holds nothing back. Format 11 |
| `/v1/work/mark-merged` | `merge_marked` | Person only in the fold too, item open and reporting a request that upstream shows neither open nor closed without a merge (`core/src/work_fold.rs#step_record`). It closes the item as merged, as a person's `closed` with `merged` already does, so it adds no power; the tracker never merges. Format 11 |
| `/v1/work/rate-summary` `{item, attempt, accurate}` | `summary_rated` | Person only in the fold too, the attempt must exist and be finished, and the same mark again is `no_change` (`core/src/work_fold.rs#step_record`). A closed item can still be marked. It changes nothing the fold acts on: no stage, claim or wait. Format 11 |
| `/v1/work/take` `{item}` | `taken` | Person only in the fold too, item open and accepted, and already the caller's is `no_change` (`core/src/work_fold.rs#assign`). The assignee is the token's person, so a person can only take for themselves. It keeps the item out of Ready, so no agent claims it, and leaves any hold as it is (`core/src/work_fold.rs#needs_you`). Format 12 |
| `/v1/work/reassign` `{item, to}` | `reassigned` | Person only in the fold too, same conditions as a take (`core/src/work_fold.rs#assign`). `to` must be a person of the token's team, by its token file or the org log, or the call is `not_member`, on this route and on `/v1/work/records` alike (`tracker/src/http.rs#as_person`); the fold does not read the org log. `to` may be an agent of the team that is not revoked, also checked there; it is `bad_assignee` on a log below format 13 (`core/src/work_fold.rs#step_record`). Only a person assigns an agent, so an agent cannot route work to itself or away from another. Format 12, an agent from 13 |
| `/v1/work/steer` `{item, attempt, text}` | `steered` | Person only in the fold too, item open and held, and `attempt` must be the one held (`core/src/work_fold.rs#step_record`). Text over the inline limit is refused, as for a note (`tracker/src/tracker/tools.rs#check_sizes`). It adds a note to that attempt, as `human_input` with an attempt does, so it gives a person no power a note does not. Format 12 |
| `GET /v1/work/queue` | None: reads the fold | Person only. Lists what waits on a person, oldest wait first, then tier, at most 1000 entries per call (`?limit=`, default 200) (`core/src/work_fold/human_wait.rs#State::queue_for`). An item assigned to another person is left out; that hides nothing, since `GET /v1/work/items` lists every item of the team. An item a person snoozed or parked is left out while the wait they hid continues. A read, so no request key |
| `GET /v1/work/items`, `GET /v1/work/items/{item}` | None: reads the fold | A person sees every item of the team, as the dashboard does from `work.events` (`tracker/src/http.rs#items`) |
| `GET /v1/work/agents` | None: reads the org log and token file | Person only. The names of the team's agents that are not revoked, so a person can pick one to assign; no token hash, holds or who added it, which stay on the org admin's `GET /v1/teams/{team}/agents` (`tracker/src/http.rs#team_agents`) |
| `GET /v1/work/items/{item}/history` | None: reads `work.events` | Records about the item, at most 500 per answer and 20,000 read, page by page from Felix (`tracker/src/tracker/reads.rs#HISTORY_PAGE`). Payloads come back as logged, with no redaction at read time. Secrets and agent text are cut only in records written since the log started redacting, so older records return as written. The same person can already read `work.events` itself, so this shows nothing new |
| `GET /v1/work/metrics/time-to-human` | None: reads the fold | Counts and percentiles of how long items waited on a person, for the whole team, from records a person can already read. No item ids and no names of who acted, so it adds no per-person figure. `days` is clamped to 1 to 366 and the answer is built from at most 20,000 kept waits (`core/src/work_fold/human_wait.rs#KEPT_WAITS`) |
| `GET /v1/work/metrics/summary-trust` | None: reads the fold | Counts of the summary marks a person wrote in a window and of finished attempts with no mark, for the whole team, from records a person can already read. No item ids and no names of agents, models or who marked, so it adds no per-person figure; a team with one reviewer still shows that reviewer's marks. `days` is clamped to 1 to 366 and the answer is a fixed set of integers (`core/src/work_fold/trust.rs#summary_trust`). A mark counts once, by its latest write |

### The dashboard

The dashboard's writes are the person routes above, called from the page with the
signed-in person's own token: `proposals/accept`, `proposals/reject`, `tier`, `close`
and `questions/answer`, plus `GET /v1/me` to check the token (`dashboard/src/tracker.ts`).
The page has no authority of its own; the tracker's checks are the only checks. A write the tracker does not answer within 30 s is shown as failed with its outcome unknown, because the record may have been appended.

| Attack | Check |
|---|---|
| A crafted link sends the person's token somewhere else | The page calls only its own origin's `/v1/...` routes; no parameter names the tracker. `?team=` only picks the `/v1/teams/{team}/` path, and the tracker checks membership (`dashboard/src/writes.ts`) |
| A token left behind in the browser | Typed into a password field, kept in memory for the tab, never in the address, local storage or the view state, and dropped by Sign out in the sidebar, which also names who is signed in, or by a 401 or 403 (`dashboard/src/main.ts`) |
| Another site makes writes as the person (CSRF) | The token goes in an `Authorization` header the page sets, not a cookie, so a request from another origin carries nothing |
| An upstream title runs as script in the page | Views build elements with `dashboard/src/ui/dom.ts`, which never parses HTML |
| A bulk action does more than the person could | Each item is its own call through the same route and checks, one after another within the action, and only on marked items the view shows (`dashboard/src/list.ts`) |
| An undo overwrites someone else's change | Only a tier change has an undo, a new `tier_changed`, and the page offers it only while its own view of the log shows the tier it set. The tier route itself is unconditional, so a change that lands between that look and the undo is overwritten; the log keeps both |

### Ingest routes and health

The ingest routes need an ingest token (`tracker/src/http/ingest.rs#worker`).

| Route | What ingest controls | Check |
|---|---|---|
| `/v1/ingest/records` | A whole record | Only the kinds upstream sync needs (`tracker/src/tracker/ingest.rs#allowed`). Only on roots it imported (`tracker/src/tracker/ingest.rs#ingest_record_in`). An `imported` naming an existing prerequisite is held to the fold's format 6 checks; ingest opens that issue, and trusts a `kbl-prerequisite:` marker, only in a repo whose own rule has `open_prerequisites`, which needs issues: write there (`ingest/src/sync.rs#opening_rule`). A marker counts only in an issue by one of `own_logins`, so those credentials must not be given to agents with issue write. A worker tied to a source by `ingest_sources` in the token file acts only on that source's roots and imports only that source's issues (`tracker/src/tracker/ingest.rs#ingest_record_in`). The `github` worker is the one exception: it may write `pr_status`, and nothing else, on a root from any source, so a Jira root's merge can be recorded (a request that opens also gets the tracker's own two review items under the root); a root's own worker still decides what upstream says about it. Once any worker is tied the tracker refuses to start unless all are, and a tied worker other than `github` lists only its own source's roots (`tracker/src/tracker/ingest.rs#ingest_roots`); with none tied, a token reaches every root. Closing as merged needs a merged status for the request the item reports (`tracker/src/tracker/ingest.rs#ingest_record_in`, `core/src/work_fold.rs#step_record`). So does `shipped`, which is also refused once a release is recorded (`tracker/src/tracker/ingest.rs#ingest_record_in`). An `unlinked` removes only an edge that ingest's own `linked` created, which the tracker derives from the log, so an edge from a prerequisite, an answer, a person or an agent stays; it is refused as `not_linked` or `not_ingests`, and ingest's `linked` of such an edge as `already_linked` (`tracker/src/tracker/ingest.rs#ingest_record_in`); ingest writes one only from a complete listing of the issue's links (`ingest/src/sync.rs#link`). An `accepted` with `triage_skipped` is a rule's choice to admit a root with no triage child; it reaches only the worker's own source's roots like every other record (`tracker/src/tracker/ingest.rs#ingest_record_in`). Context text up to 4 KiB (`tracker/src/tracker/ingest.rs#ingest_record_in`); a context write with the text the entry already holds, on an entry no agent wrote, is not logged, so repeated lost saves cannot grow the log; the stored text is read back with no lock held, a read that fails refuses the write, and an entry whose stored text retention trimmed has its current text written, since there is nothing to compare and skipping would lose it (`tracker/src/tracker/ingest.rs#ingest_record_in`). Other strings are held to 4 KiB by the append check (`tracker/src/tracker.rs#append_expecting`); ingest cuts titles itself (`ingest/src/sync.rs#cut_title`) |
| `/v1/ingest/roots` | Nothing | Read only |
| `POST /v1/org/records` | `format_raised`, `connection_set` or `budget_set` from someone who is not an org admin, or another kind past its own route's checks | Org admins only, checked before the body is read; any other kind is refused with `kind_not_allowed`; 512 KiB body (`tracker/src/http/org.rs#org_record`) |
| `GET /v1/ingest/backoff/{pool}`, `PUT /v1/ingest/backoff/{pool}` | A token holding a pool off that its team does not spend, a long hold, or a pool entry that grows the cache | The caller must be an ingest token whose team has a connection in the pool, and a token tied to a vendor only that vendor's (`tracker/src/http/ingest.rs#pool_for`, `core/src/org_fold.rs#team_spends_pool`). The wait is capped at an hour and the later time wins, written with a version check, so a shorter wait never shortens one (`tracker/src/org/backoff.rs#MAX_BACKOFF_MS`, `tracker/src/org/backoff.rs#raise_pool_backoff`). One entry per pool that a connection names, so the table is bounded by the org's own pools. Answers are relative, so no worker's clock is trusted. A token of a team in the pool can stop that pool for an hour; every worker in a pool shares one vendor limit and is trusted with it |
| Ingest request budget | Two workers spending one pool past its limit, or a stale share | A worker's share is the pool's hourly budget times the sources it holds over the sources the pool serves, and a source it is handing over is not counted, so the shares of workers that have heard their leases sum to at most the pool (`ingest/src/budget.rs#Budget::hourly`, `ingest/src/lease.rs#Lessee::serving`). A worker with no lease has a share of one request an hour and polls nothing. The source count comes back on every heartbeat answer, read with the leases it renews, so a source added or removed in the pool changes the denominator at the same moment as the leases and a count from an older config answer cannot overspend (`tracker/src/http/ingest/leases.rs#heartbeat`, `ingest/src/budget.rs#Budget::set_leases`). A stale share lasts to the next heartbeat, 30 s give or take 20%; the pool's kept-back fifth absorbs it. Wakes spend only the wake part of the kept-back share (`ingest/src/budget.rs#Budget::read_floor`) |
| `GET /v1/ingest/config` | Another team's or group's connection, or a held poll that pins the tracker | The connection is looked up in the caller's own team only, by the group it names, and a token tied to a vendor sees only a connection to that vendor (`tracker/src/http/ingest.rs#connection_for`). The `ETag` is a digest of the answer, so another team's records do not wake it into a full answer. A held poll gives back its team call slot and waits on one watch of the org view (no Felix reads). At most 256 are held per team and 1,024 in all; past that a poll is refused with `429` and `Retry-After`, so one team cannot take every slot and no worker spins. A view the org lease no longer backs answers `503` (`tracker/src/http/ingest.rs#config`) |
| `PUT /v1/ingest/leases` | A worker taking a source another worker polls, another team's source, or growing the live-worker table | The sources come from a connection of the caller's own team naming the group, and the leases sit in that team's namespace (`tracker/src/http/ingest/leases.rs#heartbeat`). A lease is written only with a version check and moves only once released or past its 90 s expiry and a further 30 s take margin, with the epoch raised on every take (`core/src/lease.rs#step`, `core/src/lease.rs#next`, `core/src/lease.rs#Lease::free`). Group membership is one version-checked cache entry, so every tracker assigns the same way, and a leave releases by the leases the cache records for the worker, even when its membership write fails (`tracker/src/http/ingest/leases.rs#join`, `tracker/src/http/ingest/leases.rs#heartbeat`). A leave also marks that worker run as gone for 10 minutes, so a renew it sent earlier that lands late cannot bring it back (`tracker/src/http/ingest/leases.rs#Membership`). The instance is agent-free worker input; at most 256 live workers per group, past that `429` (`tracker/src/http/ingest/leases.rs#MAX_MEMBERS`). Expiry is by the tracker's wall clock. Assumption: tracker clocks agree within 30 s; the take margin is the headroom for that, and for requests still in flight |
| Ingest upstream calls (lease fence) | Two workers calling the vendor for one source across a lease handover | Every vendor HTTP request passes the worker's fence in the transport (`ingest/src/lease.rs#Gate::send`): none once its last successful renew, timed from when it was sent, is 60 s old by the monotonic or the wall clock, whichever ran further (so a suspend counts; `ingest/src/lease.rs#Lessee::left`), and none into a connection source it does not hold, or into a repo the connection does not name, except a `GET` about a pull request there (`ingest/src/lease.rs#Lessee::open_for`, `ingest/src/lease.rs#read_elsewhere`). Repos match sources as the vendor compares names. A comment write-back the fence cut short has an unknown outcome: it is retried without counting a try, and the retry looks for the comment's hidden mark before it posts (`ingest/src/source.rs#mark`). A request in flight is cancelled when the fence closes, so paging stops at it. A worker stops its heartbeat before it sends its leave (`ingest/src/lease.rs#leave`). 60 s is 30 s inside the tracker's expiry, and a new holder waits a further 30 s take margin |
| `GET /v1/ingest/state/{source}`, `PUT /v1/ingest/state/{source}` | The saved cursor for a source | The name is a key (`tracker/src/tracker/ingest.rs#ingest_state`). 4 MB at most (`tracker/src/tracker/ingest.rs#ingest_save`). Saved only if nobody saved since the version read (`tracker/src/tracker/ingest.rs#ingest_save`). A token tied to a source by `ingest_sources` reads and saves only that source's state and is refused with 403 for another's (`tracker/src/http/ingest.rs#load`); an untied token reaches any source's state, which is fine with one worker |
| `/healthz` | Nothing | No token. Answers `ok` and nothing else |
| `GET /` | Nothing | No token. Answers a fixed JSON naming the service, its version and `/healthz`; no config, tokens or state (`tracker/src/http.rs#index`) |

### Team routes

Every route above acts in one team, the team of the token, and only reaches
that team's tracker: its fold, its lease and its Felix connections
(`tracker/src/teams.rs`). The team comes from the token's entry, never from the
body (`tracker/src/http.rs#from_request_parts`). Today there is one team, `KBL_NAMESPACE`,
and the token file's callers belong to it.

| Route | Fields the caller controls | What they could steer | Check |
|---|---|---|---|
| `/v1/teams/{team}/{*rest}` | `team`, and the inner route's own fields | Act in a team the token is not in; reach a route that has no team form | Only `work/` and `ingest/` routes are served under it (`tracker/src/http.rs#in_team`). The named team must be one the token belongs to; any other, existing or not, answers `404` with `unknown_team`, and the token is checked first (`tracker/src/http.rs#from_request_parts`). The route's own token and field checks then apply as above. Tested with two teams in `tracker/src/http/tests.rs` |
| `/v1/me` | None | Learn other teams | Person token only. Answers only the teams the token is in (`tracker/src/http.rs#me`) |
| `GET /v1/org/teams`, `POST /v1/org/teams` | `team`, `display_name` on `POST` | Create a team, or adopt a namespace that already holds another team's data; list the org's teams | Org admin only: a person of the token file. An agent or ingest token gets `403` with `org_admin_required` before the body is read, and the body is capped at 4 KiB (`tracker/src/http/org.rs`). The name follows the key rule and is not `kbl`. A name already on `org.events` writes nothing and answers `created: false`. The tracker only serves the namespace: Felix grants on it come from `kbl-tool team create`, run with the org admin's own Felix sign-in. Without `KBL_ORG_NAMESPACE` it answers `404` with `teams_off` |

A person token in more than one team gets `400` with `team_required` on a path
without a team. A team takes at most `KBL_TEAM_INFLIGHT` calls at once and
answers `503` with `team_busy` over that (`tracker/src/teams.rs#admit`), so one
team's load does not hold the others' calls; waiting `work_next` calls have
their own pool, so they cannot starve a holder's `extend`. A slot is taken
before the body is read, so a slow upload holds one of its own team's slots
only. A team that loses its lease answers `503` alone. Without teams on, the process
exits when every team has lost its lease. With teams on, a team that lost its
lease, or could not start (its log has a record of a newer format), answers
`503` with `team_unavailable` and is started again in the background with a
fresh fold; the process exits only when the org lease has been lost for 60
seconds. Either way it exits at once if a team's maintenance panics
(`tracker/src/teams.rs`).

Each team's tracker, and the org writer, exchanges its ID token for Felix
tokens narrowed to its own namespace (`tracker/src/felix.rs`), so a record
sent on the wrong team's connection is refused by the broker.

## Metrics endpoint

Each service can serve Prometheus metrics on a port of its own, off until an
address is set ([metrics.md](metrics.md)). The API listener never serves it.

| Route | Who can call it | What it could steer | Check |
|---|---|---|---|
| `/metrics` | Anyone who can reach the port. No token | Nothing: it is read only. It could leak what the metrics hold, or be used to count calls and watch timing | Loopback only unless `--metrics-allow-remote` says the port is the scraper's alone (`metrics/src/serve.rs#check_listen`). The flag is a promise the process cannot check. Labels are route templates, tool names, the source adapter's name, team names and fixed result words, so an item id, title, agent or person name, repo, issue text and token never reach the exposition (`tracker/src/http.rs#observe`, `mcp/src/metrics.rs#tool_finished`). A client cannot mint labels: an unknown tool is `unknown` and an unusual method is `other`. Residual: team names and per-route counts are visible to a scraper, and a scraper learns when agents are busy |


## Ingest

| Route | Who can call it | What it could steer | Check |
|---|---|---|---|
| `/webhook` | Anyone who can reach the port | Make ingest spend GitHub requests; inject data | A webhook carries no data into the tracker, it only wakes the poll. HMAC signature, and an empty secret refuses everything (`ingest/src/webhook.rs#hook`). A repeated delivery id is dropped (`ingest/src/webhook.rs#hook`), which stops GitHub's own redeliveries and nothing else: the signature covers the body, not the delivery id, so a captured body can be sent again under a new id. Without TLS in front, anyone on the path can capture one; ingest listens only on loopback unless a TLS proxy is declared. What bounds a replay is that wakes are spaced (`ingest/src/webhook.rs#hook`) and a wake only starts a poll |

### What ingest believes

An agent names its pull request in `/v1/work/progress`. Ingest reads that
request only when all of these hold, so the name alone steers nothing.

| Attack | Check |
|---|---|
| Name a request in another repo | Only the issue's own repo, and only a repo an intake rule names (`ingest/src/sync.rs#may_write_to`) |
| Name a stranger's request | Only a request one of our own logins opened (`ingest/src/sync.rs#linked_request`, `ingest/src/github.rs#pull_request`, `ingest/src/gitlab.rs#pull_request`) |
| Name an old merged request | Created after the item's first claim, and tied to the item by the branch `kbl/<item>` or a closing reference (`ingest/src/sync.rs#linked_request`) |
| Name a request for a Jira root so the GitHub worker records its merge | The same author, age and default-branch checks, but the branch only (a closing reference names GitHub issues), in a repo one of the GitHub worker's own rules names (`ingest/src/sync.rs#foreign_pull_requests`). The worker writes `pr_status` and nothing more; the tracker refuses the rest of its records for a root of another source (`tracker/src/tracker/ingest.rs#ingest_record_in`), and a status for a request the item did not report as `wrong_pr` (`core/src/work_fold.rs#step_record`) |
| Merge into a side branch, or merge its own work | A merge counts only into the default branch and not by one of our logins (`ingest/src/github.rs#pull_request`); on GitLab also a named merger (`ingest/src/gitlab.rs#pull_request`) |
| Approve its own work | Reviews count only from owners, members and collaborators, never the author (`ingest/src/github.rs#review_state`) |
| Post a green status itself | Statuses from our own logins are ignored; a failed or partial read is pending, never green (`ingest/src/github.rs#check_state`) |
| Post a green check run with our own App's token | A check run by an App whose `<slug>[bot]` is one of our logins is ignored (`ingest/src/github.rs#check_state`) |
| Name another item's request in other letter case | The tracker records the casing an open item already uses, so the fold refuses it as `pr_in_use` (`tracker/src/tracker.rs#canonical_pr`) |
| Get a stranger's request turned into a draft | Write-back uses the same repo and author checks (`ingest/src/sync.rs#write_back`) |
| Hide instructions in an issue or comment | Not stopped: the text is the work. Intake rules choose whose issues get in, text is cut to fit a record (`ingest/src/sync.rs#cut`), our own comments are skipped (`ingest/src/sync.rs#on_comment`), and an edit after triage shows as "changed upstream" |

## MCP tools

`kbl-mcp` runs on the agent's side. The work tools call the tracker with the
agent's token, so the tracker row is the check. The task tools write to Felix
with the agent's Felix token, so Felix grants are the check.

| Tool | Goes to | Check |
|---|---|---|
| `work_next` | `/v1/work/next` | Tracker row |
| `work_peek` | `/v1/work/peek` | Tracker row. It reads the ready items and writes nothing |
| `work_extend` | `/v1/work/extend` | Tracker row |
| `work_complete` | `/v1/work/complete` | Tracker row |
| `work_release` | `/v1/work/release` | Tracker row |
| `work_post` | `/v1/work/post` | Tracker row |
| `work_link` | `/v1/work/link` | Tracker row |
| `work_context` | `/v1/work/context` and `/v1/work/context/add` | Tracker rows. Artifacts are shown only if the bytes hash to the id (`mcp/src/tasks.rs#artifact_text`) |
| `work_progress` | `/v1/work/progress` | Tracker row |
| `work_finding` | `/v1/work/finding` | Tracker row. The tool also limits `severity` and `verdict` to their words (`mcp/src/server.rs#item_or_held`) |
| `work_triage` | `/v1/work/triage` | Tracker row. The tool limits `unclear` to its three words |
| `work_verdict` | `/v1/work/verdict` | Tracker row. The tool limits `verdict` to merge or block |
| `decide` | `/v1/work/decide` | Tracker row. Known secret patterns are cut before long text is stored as an artifact (`mcp/src/tasks.rs#content`) |
| `record_step` | `/v1/work/step` | Tracker row. Known secret patterns are cut before long text is stored as an artifact (`mcp/src/tasks.rs#content`) |
| `edit` | `/v1/work/edit` | Tracker row. Known secret patterns are cut before long text is stored as an artifact (`mcp/src/tasks.rs#content`) |
| `work_ask` | `/v1/work/ask` | Tracker row |
| `work_answer` | `/v1/work/answer` | Tracker row |
| `work_rules` | `/v1/work/rules/read` | Tracker row |
| `run_info` | Nothing | Reads its own settings |
| `task_post` | The run's tasks stream and run log | **None** beyond access to the run ([#29](https://github.com/GetFelix/KBL/issues/29)). Known secret patterns are cut from the title (`mcp/src/tasks.rs#post`) and the body (`mcp/src/tasks.rs#content`) |
| `task_claim` | The run's `workers` group | Felix hands a task to one member. An artifact body is shown only if its bytes hash to the id (`mcp/src/tasks.rs#artifact_text`). The two-attempt limit is in `kbl-mcp` only (`mcp/src/tasks.rs#claim`, [#39](https://github.com/GetFelix/KBL/issues/39)). A task of a format newer than the worker reads is handed back, not run (`mcp/src/tasks.rs#claim`); a writer in the run can post one, and each hand-back uses a delivery until Felix dead-letters it. Known secret patterns are cut from the error text of every `refused` record, the dead-letter and run log gap notes included (`mcp/src/tasks.rs#refuse`) |
| `task_complete` | The results cache, artifacts and run log | First result wins by a put-if-absent in Felix. Who may write: **None** (#29). Known secret patterns are cut from the result (`mcp/src/tasks.rs#complete`) |
| `task_release` | The `workers` group and run log | `kbl-mcp` only releases the task it holds (`mcp/src/tasks.rs#holding`). In Felix: **None** (#29). Known secret patterns are cut from the reason (`mcp/src/tasks.rs#release`) |
| `task_give_up` | The `workers` group and run log | Same, and the reason too (`mcp/src/tasks.rs#give_up`) |

## Person MCP tools

`kbl-mcp person` is a second server, for a person's own MCP host. It is started
with a person's tracker token in `KBL_PERSON_TOKEN`, a variable the agent server
never reads, and serves only the tools below. The agent server has none of them
and is never given that token, so an agent cannot reach a person route through
either server. `kbl-mcp person` refuses to start unless the tracker names the
token's owner as a person (`mcp/src/person.rs#connect`), so an agent's
token yields no tools. Its calls go to the same routes the dashboard uses with
that token: the tracker refuses an agent's token on every one of them
(`tracker/src/http.rs#person`), writes the record under the person's name,
and keeps the call inside the token's team (`--team` puts it in the path). A
tool refuses a parameter it does not list and a value of the wrong kind before
the call is made (`mcp/src/person.rs#check`).

| Tool | Goes to | Check |
|---|---|---|
| `my_queue` | `GET /v1/work/queue` | Person only (`tracker/src/http.rs#person`) |
| `accept_proposal` | `POST /v1/work/proposals/accept` | Person only (`tracker/src/http.rs#person`) |
| `reject_proposal` | `POST /v1/work/proposals/reject` | Person only (`tracker/src/http.rs#person`) |
| `answer_question` | `POST /v1/work/questions/answer` | Person only (`tracker/src/http.rs#person`) |
| `set_tier` | `POST /v1/work/tier` | Person only (`tracker/src/http.rs#person`) |
| `flag_item` | `POST /v1/work/flag` | Person only (`tracker/src/http.rs#person`) |
| `unflag_item` | `POST /v1/work/unflag` | Person only (`tracker/src/http.rs#person`) |
| `add_note` | `POST /v1/work/note` | Person only. Known secret patterns are cut by the tracker before the note is stored (`tracker/src/http.rs#note`). Text over the inline limit is refused by the tool, since this process has no Felix to store an artifact in |
| `redo_from_decision` | `POST /v1/work/redo` | Person only (`tracker/src/http.rs#person`) |
| `retry_item` | `POST /v1/work/retry` | Person only (`tracker/src/http.rs#person`) |
| `snooze_item` | `POST /v1/work/snooze` | Person only (`tracker/src/http.rs#person`) |
| `block_item` | `POST /v1/work/block` | Person only (`tracker/src/http.rs#person`) |
| `lift_block` | `POST /v1/work/lift-block` | Person only (`tracker/src/http.rs#person`) |
| `lift_resource` | `POST /v1/work/lift-resource` | Person only (`tracker/src/http.rs#person`); one record lifts every block on one resource |
| `classify_item` | `POST /v1/work/classify` | Person only (`tracker/src/http.rs#person`); the fold refuses a value outside the team's vocabulary, and a person's value is never replaced by triage or a rule |
| `park_item` | `POST /v1/work/park` | Person only (`tracker/src/http.rs#person`) |
| `unpark_item` | `POST /v1/work/unpark` | Person only (`tracker/src/http.rs#person`) |
| `unblock_item` | `POST /v1/work/unblock` | Person only (`tracker/src/http.rs#person`) |
| `relate_items`, `unrelate_items` | `POST /v1/work/relate`, `/v1/work/unrelate` | Person only (`tracker/src/http.rs#person`) |
| `request_review` | `POST /v1/work/request-review` | Person only (`tracker/src/http.rs#person`) |
| `request_changes` | `POST /v1/work/request-changes` | Person only (`tracker/src/http.rs#person`) |
| `mark_merged` | `POST /v1/work/mark-merged` | Person only (`tracker/src/http.rs#person`) |
| `rate_summary` | `POST /v1/work/rate-summary` | Person only (`tracker/src/http.rs#person`) |
| `take_item` | `POST /v1/work/take` | Person only (`tracker/src/http.rs#person`) |
| `reassign_item` | `POST /v1/work/reassign` | Person only, and `to` a person or a live agent of the team (`tracker/src/http.rs#as_person`) |
| `steer_agent` | `POST /v1/work/steer` | Person only (`tracker/src/http.rs#person`) |

`my_queue` returns raw item text to the host model. Tool names and routes are static, so that text cannot change which calls exist, but hosts should treat it as untrusted.

## Records on work.events

Only the tracker appends. "Agent" below means a record written for an agent's
token, "tracker" one the tracker or ingest wrote, "person" one from a person's
token. A kind an actor may not write is refused with `actor_not_allowed`.

| Kind | Who may write | Check |
|---|---|---|
| `imported` | Tracker, person | Not an agent; one per upstream id (`core/src/work_fold.rs#step_record`). From format 6 it may name an existing item only if that item is an open root made by a `prerequisite` record, not a deferred finding, that a person accepted after the raise, the issue is in the repo it targets, and it has no upstream issue yet (`core/src/work_fold.rs#attach_upstream`) |
| `proposed` | All | An agent needs to hold the parent, and draws budget (`core/src/work_fold.rs#propose`). A `review` item only from the tracker, under an open root that reports the same request, 8 per root (`core/src/work_fold.rs#check_new_review`, `core/src/work_fold.rs#check_new_review`) |
| `triaged` | Person, or the agent holding the item's triage child | `core/src/work_fold.rs#may_triage`. An agent writes it through `/v1/work/triage` |
| `accepted` | Tracker, person | `core/src/work_fold.rs#step_record`. Its `triage_skipped` field, the rule that admitted a root without triage, is accepted only from the tracker (so only ingest, under its own source's ties) and only on a root with no children; a person or an agent carrying it is `actor_not_allowed`, so no one else can make a stage look like a rule's decision. It changes no stage and is shown only (`core/src/work_fold.rs#step_record`) |
| `prerequisite` | The holding agent | `core/src/work_fold.rs#prerequisite`. Not closed, except from format 5 a `released` wait on merged or completed work with no release (`core/src/work_fold.rs#prerequisite`). With `triage`, the agent holding that triage item writes it for the item under triage, and keeps its claim (`core/src/work_fold.rs#prerequisite_holder`). Format 3, so refused until a person raises the log |
| `shipped` | Tracker, person, ingest | Only a done item (`core/src/work_fold.rs#step_record`). Ingest only on a root it imported whose reported request has a merged status, and not once a release is recorded (`tracker/src/tracker/ingest.rs#ingest_record_in`). The worker, not the tracker, checks that the release is published and not a draft or prerelease, and that GitHub says its tag contains the merge commit. A person's `shipped` replaces the release recorded before, ingest's included (`core/src/work_fold.rs#step_record`), and a reopen clears it (`core/src/work_fold.rs#step_record`) |
| `linked` | All | An agent only inside one tree (`core/src/work_fold.rs#agent_may_link`) |
| `unlinked` | Tracker, person | `core/src/work_fold.rs#step_record` |
| `related`, `unrelated` | Person | Items exist, an item is not related to itself, at most 20 per item (`core/src/work_fold.rs#step_record`). Format 18, so refused until a person raises the log. Holds nothing back |
| `context` | All | An agent only on work it holds (`core/src/work_fold.rs#step_record`). An agent replaces only its own entries (`tracker/src/tracker/tools.rs#context_add`) |
| `finding` | The agent holding a review item | `core/src/work_fold.rs#finding`. Bounded and capped as the route above |
| `review_verdict` | The agent holding a final review | `core/src/work_fold.rs#step_record`, `core/src/work_fold.rs#step_record`. Open fixes block a merge (`core/src/work_fold.rs#step_record`) |
| `claimed` | Agent, written by the tracker on a delivery | `core/src/work_fold.rs#step_record` |
| `progress` | Holder | `core/src/work_fold.rs#step_record` |
| `done` | Holder | `core/src/work_fold.rs#step_record` |
| `released` | Holder, or the tracker on a lapse | `core/src/work_fold.rs#step_record` |
| `gave_up` | Holder | `core/src/work_fold.rs#step_record` |
| `question` | The holding agent | `core/src/work_fold.rs#step_record` |
| `answer` | Person, or the agent holding a question item | `core/src/work_fold.rs#step_record` |
| `decision` | The holding agent | `core/src/work_fold.rs#step_record` |
| `step` | The holding agent | `core/src/work_fold.rs#step_record` |
| `edit` | The holding agent | `core/src/work_fold.rs#step_record` |
| `human_input` | Tracker, person | `core/src/work_fold.rs#step_record` |
| `forked_from` | Tracker, person | Redo limit per item (`core/src/work_fold.rs#step_record`). While the item's pull request has a failed check, the tracker refuses one that names no note written since the failure (`tracker/src/tracker.rs#retry_has_note`) |
| `closed` | Tracker, person | The tracker closes as merged only on a merged status (`core/src/work_fold.rs#step_record`) |
| `rejected` | Tracker, person | `core/src/work_fold.rs#step_record` |
| `updated` | Tracker, person | Open roots only (`core/src/work_fold.rs#upstream_sync`) |
| `left_scope` | Tracker, person | Open roots only (`core/src/work_fold.rs#upstream_sync`) |
| `returned_to_scope` | Tracker, person | Open roots only (`core/src/work_fold.rs#upstream_sync`). Format 2, so refused until a person raises the log (`core/src/work_fold.rs#format_gate`) |
| `flagged` | Person | Open, accepted items only, and only a change (`core/src/work_fold.rs#step_record`). Format 7, so refused until a person raises the log (`core/src/work_fold.rs#format_gate`). A reopen clears it |
| `retried` | Person | Only an open item nobody holds whose open attempt used its tries (`core/src/work_fold.rs#step_record`). Format 10, so refused until a person raises the log (`core/src/work_fold.rs#step_record`) |
| `snoozed` | Person | Open items that wait on a person, ending 1 ms to 30 days after the record (`core/src/work_fold.rs#step_record`). Format 10 |
| `block_set` | Person | Open items only, with a time, a resource or both, a reason, and a plain resource name; a block that holds and says the same is not kept twice (`core/src/work_fold.rs#step_record`). Format 16 |
| `block_cleared` | Person | Open items whose block still holds (`core/src/work_fold.rs#step_record`). Format 16 |
| `resource_lifted` | Person | Ends only blocks that still hold on an open item and name exactly the resource, in one record; a lift that changes nothing is not kept (`core/src/work_fold.rs#step_record`). Format 20 |
| `parked` | Person | Open items only; parking needs an item that waits on a person, and only a change is kept (`core/src/work_fold.rs#step_record`). Format 10 |
| `unblocked` | Person | Only a link that still holds the item (`core/src/work_fold.rs#step_record`). Format 10 |
| `review_requested` | Person | Open items whose request upstream shows open and unapproved, with a reviewer name of 1 to 100 bytes and no control character (`core/src/work_fold.rs#step_record`). Format 11, so refused until a person raises the log (`core/src/work_fold.rs#format_gate`) |
| `changes_requested` | Person | Open items whose request upstream shows open and unapproved, and only a change (`core/src/work_fold.rs#step_record`). Format 11 |
| `merge_marked` | Person | Open items that report a request upstream shows neither open nor closed without a merge (`core/src/work_fold.rs#step_record`). Format 11 |
| `summary_rated` | Person | An existing, finished attempt, and only a change (`core/src/work_fold.rs#step_record`). Format 11 |
| `taken` | Person | Open, accepted items; the assignee is the writer, and only a change (`core/src/work_fold.rs#assign`). Format 12 |
| `reassigned` | Person; the tracker only to clear an agent assignee | Open, accepted items, and only a change (`core/src/work_fold.rs#assign`). An agent assignee is `bad_assignee` below format 13. Format 12 |
| `steered` | Person | Open items an agent holds, on the attempt it holds (`core/src/work_fold.rs#step_record`). Format 12 |
| `reopened` | Tracker, person | Closed roots only (`core/src/work_fold.rs#step_record`). Ingest reopens only what it closed. It keeps release waits and clears the item's release, so a waiter waits for the next release |
| `attempt_closed` | Tracker, person | `core/src/work_fold.rs#step_record` |
| `tier_changed` | Person | `core/src/work_fold.rs#step_record` |
| `edited` | Person | `core/src/work_fold.rs#step_record` |
| `budget_set` | Person | `core/src/work_fold.rs#step_record` |
| `price_set` | Person | Not backdated; usage waiting for a price is charged the rest when one comes (`core/src/work_fold.rs#step_record`, `core/src/work_fold.rs#reprice`). Format 8, so refused until a person raises the log (`core/src/work_fold.rs#format_gate`) |
| `pr_status` | Tracker, person | Only for the request the item reports, bounded and consistent (`core/src/work_fold.rs#step_record`) |
| `source_stalled` | Tracker | 200-byte names, 64 repos (`core/src/work_fold.rs#step_record`) |
| `rules_set` | Person | `core/src/work_fold.rs#step_record` |
| `refused` | Tracker | `core/src/work_fold.rs#step_record` |
| `format_raised` | Person | Only upward, only to a format the tracker's build reads, and stamped with that format (`core/src/work_fold.rs#format_gate`). An older reader stops at it ([upgrades.md](upgrades.md)) |

## Records on the run log

A run log (`agents.log.<run>`) is written straight to Felix by `kbl-mcp`,
`kbl-run` and `kbl-tool`, each with its own Felix token. Nothing
validates a record before it lands, and `by` is what the writer says. So a
run log is a set of claims, and the dashboard marks it unverified. Who may
write which kind: **None**, tracked by
[#29](https://github.com/GetFelix/KBL/issues/29) (roles per run),
[#45](https://github.com/GetFelix/KBL/issues/45) (the publisher Felix
recorded) and [#32](https://github.com/GetFelix/KBL/issues/32) (a
grants test with a raw client).

| Kinds | Meant to be written by |
|---|---|
| `run_started` | `kbl-tool` |
| `run_ended`, `agent_up`, `model_started`, `model_call`, `budget_exhausted`, `refund_unconfirmed` | `kbl-run`, with the runtime token the model cannot reach |
| `agent_gone` | The planner's `kbl-mcp` |
| `task_posted`, `task_claimed`, `task_extended`, `task_released`, `task_done`, `task_duplicate`, `task_dead`, `task_claim_lost` | An agent's `kbl-mcp` |
| `board_put`, `board_conflict`, `lease_taken`, `lease_renewed`, `lease_released`, `lease_lost`, `message`, `refused` | An agent's `kbl-mcp` |

## Teams

Partly built ([#185](https://github.com/GetFelix/KBL/issues/185),
[design.md](design.md#teams)): the team table, the team paths and `/v1/me` are
under [Team routes](#team-routes). The rows below are the checks each later
pull request in the plan has to add. They move into the tables above as the
routes ship.

### Actors with teams

| Actor | Credential | Trust |
|---|---|---|
| Org admin | A person token with `org_admin` on `org.events`, and a Felix sign-in with `tenant.manage` for `kbl-tool` | Trusted for the whole org |
| Team member | A person token, and a role in each team they belong to | Trusted to the role's limit, in their own teams only |
| Team agent | An agent token bound to one team | Not trusted, as any agent |
| Team ingest | An ingest token bound to one team | Trusted code carrying untrusted text, for the team's own sources |

The new asset is `org.events`: who is in which team, every role and who
granted it, token hashes, sources and grants. An attacker wants a role,
a team or a token it was not given.

### Org and team routes

| Route | Fields the caller controls | What they could steer | Check |
|---|---|---|---|
| `POST /v1/teams/{team}/members` | `team`, `person`, `role`, `repos`, `subject` | Make itself or a friend admin; reach another team; take over a person by naming their OIDC subject | Admin of that team or `org_admin`. `org_admin` is granted only by an `org_admin`. `repos` must be the team's own sources. A `subject` already on another person is refused. The Felix assignment is made with the caller's own Felix credential, so Felix refuses one outside the caller's scope |
| `DELETE /v1/teams/{team}/members/{person}` | `team`, `person` | Lock out the other admins | Team admin or `org_admin`. The last admin of a team cannot be removed; an `org_admin` can always add one back |
| `POST /v1/teams/{team}/agents` | `team`, `agent` | Mint tokens without limit; take an existing agent's name and its holds | Triager or above in that team. A name in use in the team is refused, and so is a revoked one, so a new token never inherits holds. A cap on agents per team |
| `DELETE /v1/teams/{team}/agents/{agent}` | `team`, `agent` | Revoke another team's agent; free work held by others | Whoever added it, or team admin, in that team only. Releases holds only of that agent |
| `POST /v1/teams/{team}/sources` | `team`, `source`, `repo` | Pull another team's issues into its own queue | Team admin. One team per source: a source another team owns is refused |
| `POST /v1/teams/{team}/ingest` | `team`, `name` | Write to another team's log | Team admin. The token is bound to the team, and ingest records only for roots from the team's sources |
| `POST /v1/teams/{team}/grants` | `team`, `from_team`, `agents`, `repos`, `max_pending` | Give itself the right to file into another team | Only an admin of the receiving team (`{team}`) writes a grant. `repos` must be that team's sources |
| `/v1/teams/{team}/work/...` | `team`, and the route's own fields | Act in a team it is not in | The person must be a member with the route's role, checked before the append. An agent or ingest token with another team's path gets `unknown_team`, the same answer as a team that does not exist |

### Attacks across teams

| Attack | Check |
|---|---|
| A team's agent or viewer reads or writes another team's streams with a raw client | Felix: every grant names the team's namespace, and no team principal has anything outside it. Tested with a raw Felix client in `dev/check-teams.mjs`: a viewer of team `alpha` reads `alpha`'s `work.events` and cannot read or write `beta`'s, and the tracker's token narrowed to `alpha` cannot reach `beta` either |
| A tracker bug routes team A's record to team B's log | Each team's fold publishes only with a Felix token narrowed to its own namespace, so the broker refuses it |
| A revoked agent keeps working | The token table is the fold of `org.events`; the next call after `agent_revoked` is refused, and its holds are released |
| A removed member keeps reading the dashboard | Removing the Felix assignment revokes the person's broker tokens and ends open streams, within the control plane's sync interval |
| A cross-team prerequisite floods another team's Triage | A grant from the receiving team is needed, the filing agent's pending-proposal caps apply, and the grant can carry its own cap |
| A forged "prerequisite done" frees blocked work | `prerequisite_settled` is written only by the tracker, from the other team's fold. No route takes it from a person, agent or ingest |
| `org.events` leaks | It holds SHA-256 hashes of 32-byte random tokens, not tokens. Only the tracker and org admins can read it |
| One team's load or a broken log stops the others | A cap on calls in flight per team, and a team with a lost lease or failing Felix calls answers 503 alone: built, see [Team routes](#team-routes). A team whose log has a newer format, or that lost its lease, answers 503 and is retried in the background while the others serve. One process is still one crash for all (#124) |
| A person in two teams acts in the wrong one | Person routes name the team; a person token in several teams gets `team_required` on a path without one |

## Records on org.events

Built in `kbl-core` ([#185](https://github.com/GetFelix/KBL/issues/185)). The tracker writes `team_created`; no route writes the others yet. The tracker will be the only appender, and
it checks the caller's role before it asks the fold. The fold (`OrgState::check`)
then refuses a record the log cannot take, before the append and again on replay, so a
refused call writes nothing and an old log folds as it did. The log holds the SHA-256 of
each token and never a token. A format above the build's stops the reader.

| Kind | Who may write | Check |
|---|---|---|
| `team_created` | Org admin, through `POST /v1/org/teams`; or the tracker, adopting `KBL_NAMESPACE` when the log is empty | Name follows the key rule and is not `kbl`; repeating a team changes nothing |
| `person_added` | Org admin, team admin (route check, later) | Name and OIDC subject unique, token hash not used by any person, agent or ingest worker |
| `role_granted` | Team admin, org admin (route check, later) | Person and team exist; `org_admin` only with no team, a team role only with one; `repos` are the team's own sources; the last unscoped admin of a team cannot be demoted; an admin role covers the whole team, so a scoped one is refused |
| `role_revoked` | Team admin, org admin (route check, later) | The last admin of a team and the last org admin stay |
| `agent_added` | Triager and above (route check, later) | Team exists; name unique in the team and a revoked name stays taken; token hash unused; 256 per team |
| `agent_revoked` | Whoever added it, team admin (route check, later) | Team and agent exist; revoking twice changes nothing |
| `source_added` | Team admin (route check, later) | One owning team per source, compared exactly (case counts); a label of 1 to 256 bytes with no control characters |
| `ingest_added` | Team admin (route check, later) | Name unique in the team, token hash unused, 32 per team |
| `grant_added` | Admin of the receiving team (route check, later) | Both teams exist and differ; `repos` are the receiver's sources; 256 per receiving team |
| `format_raised` | Org admin, through `POST /v1/org/records` (`tracker/src/http/org.rs#org_record`) | Only upward, never past the build's own format, and stamped with the format it raises to, so a reader below it stops at the raise (`core/src/org_fold.rs#check`). Every other record stamped above the log's format is refused |
| `connection_set` | Org admin, through `POST /v1/org/records` | Org format 2. Every source is the team's own, listed once, and of the connection's vendor (the vendor tie on an ingest token trusts it), so a connection cannot reach another team's repo; a source is served by one connection of its team and a group serves one connection, so one group leases it; 1,000 sources a record and 64 connections a team bound the fold (`core/src/org_fold.rs#check`) |
| `budget_set` | Org admin, through `POST /v1/org/records` | Org format 2. Calls per hour above 0, the kept-back part no larger than it, the wake cap no larger than the kept-back part |

## Attack classes found in review

| Class | Example | Where it is stopped |
|---|---|---|
| Steering a field the agent does not own | Set rules, tiers or budgets; name itself on a record; move work to another repo | Role and name come from the token; person-only kinds; `moves_to` refused |
| Cross-tree links | Make another tree wait on its item | `/v1/work/link` is one tree only; a prerequisite needs a grant |
| Writes to work it does not hold | Context, progress or an answer on another agent's item | Holder checks in the fold on every agent record |
| Unbounded writes per attempt | Loop on `decide` or `record_step` to fill the log | A cap per kind per attempt, two claims per attempt, and one logged refusal |
| Forged artifacts and statuses | Bytes under another id; a green check; an approval; a release | Hash check on read; statuses come from ingest only; `shipped` is not an agent's |
| Merges into other branches | Merge into a side branch and report the item merged | A merge counts only into the default branch, by someone else |
| Budget escapes | Free questions, free prerequisites, resume work past the depth limit | Cost is charged on ask and prerequisite; depth applies to resume work. Every claim end and ask is charged at least the floor the tracker writes on it, so an agent that reports no usage still spends the root's cost budget. Naming a model with no price costs the floor only until a person prices it; then the fold draws the rest. At most 32 models wait for a price at once, so made-up names cannot grow the state without bound. Naming a cheaper model than the one used, or under-reporting tokens, is not caught until `kbl-run` measures usage |
| Secrets in what an agent reports | A token from the environment in a step's output or a diff hunk, kept on the log for as long as the item | Known secret shapes are replaced with `[redacted]` by the tracker in the free text of every record an agent writes (`tracker/src/tracker.rs#append_expecting`) and of a person's note (`tracker/src/http.rs#note`) or answer (`tracker/src/tracker/tools.rs#person_answer`), and by `kbl-mcp` before it stores an artifact (`mcp/src/tasks.rs#content`), a task result (`mcp/src/tasks.rs#complete`) or a task title (`mcp/src/tasks.rs#post`). Every record kind's fields are sorted into redacted and structured, with no default (`core/src/redact.rs#redact_record`); a progress record's `pr` and `branch` are keys the fold matches on, so only URL userinfo is cut from them. Each shape is matched on what a real one looks like, not its prefix alone, because a cut word in a record is lost for good. What still goes through: a secret with no known prefix, such as an AWS secret access key; `Bearer` followed by two spaces or a tab; quoted assignments such as `TOKEN="..."` and `"token": "..."`; `password=`; `token=` outside a URL query unless the value has a long run of mixed letters and digits; the body of a key whose lines start with anything but indentation, `>`, `+` or `-`, such as `# ` or `//` comments, or that has a `Name: value` header line over 256 bytes or one that names another key header (its header line is cut); an AWS access key id followed by anything but a second id, such as `AKIA...EXAMPLEs`; a `Bearer` token with no digits outside an `Authorization` header, or inside one when it reads as a call, a path or words joined by `-` (`Bearer correct-horse-battery-staple-token` and `Bearer AbCd.EfGh` look like prose and paths), after `AUTHORIZATION,` with no quote, or in `from_static("Bearer ...")` and `format!("Bearer ...")`, where no header name is in sight; a PEM body indented more than 128 bytes with no `END` line; a key with no `END` whose first body line is under 32 bytes, or under 64 bytes with fewer than three character classes (a 64-character hex hash after a header is cut with one more short line), whose first line is over 71 bytes with fewer than three character classes, or that has more than one blank line after its header; a one-line key with no `END` whose short last run has no `=` padding and is followed by prose; a Slack token whose secret is under 12 bytes or of one character class. The reverse also happens: `feature/xoxb-2-RotateTokensV2x` is cut as a token and `-----BEGIN PRIVATE KEY-----` named in prose is cut as a header, which loses the words and nothing else. Line breaks escaped to any depth, written as `\u000a` or `%0A` (with or without the carriage return, and with `%2B`, `%2F` and `%3D` read as base64), and a `\n\` continuation, are followed through a key body. `PRIVATE KEY` and PGP `PRIVATE KEY BLOCK` headers are keys, also URL-encoded with `+` or `%20` for the spaces (`-----BEGIN%20PRIVATE%20KEY-----`), `Proc-Type:`, `DEK-Info:` and any number of PGP armor lines before a body go with it, a `/` escaped as `\/` and `+`, `/` or `=` written as `\u002b`, `\u002f` or `\u003d` (at any depth) count as base64, a URL-encoded key pasted on one line has its runs split by `%20`, a body with its line breaks stripped goes whole, stuck to the header or after one gap, a URL-encoded byte (`%20ghp_...`, `d%3Dxoxd-...`) starts a secret like a space does, and two AWS ids joined by one byte are cut together |
| Metrics that carry content | An item title, a token or an agent's name in a label, read by anyone who can scrape | Labels are fixed words and route templates; the tests scrape after calls that carry known secrets and fail if one appears |
| Forged webhooks | Wake ingest in a loop | Signature and spacing. A captured body can be replayed under a new delivery id, and it only wakes the poll |

## Assumptions

| Assumption | What breaks without it | Check |
|---|---|---|
| Felix never cuts a record it acknowledged on `work.events`. A failover that did, with another writer refilling the offset, leaves the log ending where the tracker expects, so a resume or an extend could hand out a claim another agent holds | Two agents work one attempt | At start the tracker refuses a `work.events` that is not durable and either `Quorum` or one copy (`tracker/src/felix.rs#acks_are_durable`). A single copy also needs the broker to fsync before it acks, which the tracker cannot read ([felix#2735](https://github.com/GetFelix/felix/issues/2735)). With one copy this also relies on a record Felix never acknowledged not being readable, which holds under fsync `on_commit`. The tracker repeats the stream check on every rebuild and refuses work while it fails. KBL does not defend against Felix losing an acknowledged write |
| A read of the log's tip on `work.events` shows every acknowledged write. Felix does not promise this once `lease_free_reads` is final: a deposed `Quorum` leader may serve a read that shows less ([felix#2735](https://github.com/GetFelix/felix/issues/2735)) | A resumed or extended claim is reported live briefly after its hold ended | Wasted agent work only, never two holders: every claim is conditional on the tail, so Felix refuses it and the tracker rebuilds (`tracker/src/tracker.rs#hold_at_tip`) |

## Gaps

| Issue | Gap |
|---|---|
| [#109](https://github.com/GetFelix/KBL/issues/109) | Proposals draw no budget; post keys can be reused across claims |
| [#185](https://github.com/GetFelix/KBL/issues/185) | `org.events` has no record to remove a person, rotate a person's token or revoke an ingest token yet; counts of teams, people and members are not capped |
| [#120](https://github.com/GetFelix/KBL/issues/120) | No roles: every person can do everything, every agent can claim any repo's work |
| [#121](https://github.com/GetFelix/KBL/issues/121) | Tokens are long-lived |
| [#29](https://github.com/GetFelix/KBL/issues/29), [#32](https://github.com/GetFelix/KBL/issues/32), [#45](https://github.com/GetFelix/KBL/issues/45) | Any writer in a run can write any run log record under any name |
