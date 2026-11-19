# A work tracker for coding agents

It applies the Felix pieces in
[design.md](design.md) to a job that never ends: bugs arrive from an upstream
tracker, agents pick up the triage, and coding agents ask for the next piece
of work, fix it and hand it back.

**The tracker is not agentic.** KBL (Kanban By Log, pronounced Kibble) is a Felix-backed work tracker. It stores,
orders, hands out and displays work, and it makes no model calls and writes no
text. Every kind of judgement it shows (triage, augmentation, splits, answers,
fixes, redos) is a work item that an agent or a person completes. Agents from
any vendor take that work through `kbl-mcp`, and people guide it from the
dashboard. Anything a person adds (a redo, a note, a split, a question, a
priority change) becomes regular work or a record the tracker already keeps.
`kbl-mcp` and the dashboard make no model calls either.

## The problem

Bugs arrive in GitHub Issues, GitLab, Azure DevOps or Jira. Agents fix them, but three
things are missing:

- **Asking for the next piece of work.** An agent has no way to say "give me
  the most important item I can start now" and know nobody else got it.
- **Seeing the queue.** What is waiting, what is blocked, who holds what, and
  what an agent did with it.
- **Dependencies.** Item B cannot start until A is done. A large bug splits
  into chunks that run in order or in parallel.

People also need to add what they know about a bug, and agents need to read
it before they start.

## What it is

A service, `kbl-tracker`, with a REST API for people and tools and the same
operations as MCP tools in `kbl-mcp` for agents:

| Tool | Does |
|---|---|
| `work_next` | Claims the highest-priority ready item the agent can take, roots taking turns, optionally narrowed by kind, tier, repo or excluded roots. The reply carries the [standing rules](#standing-rules) in effect and a `loop` line telling the host to call `work_next` again after each item and to stop when nothing is ready. With `review_tiers` it also takes [reviews](#review-as-work) of those tiers |
| `work_peek` | Lists the next items `work_next` would hand out, in order, with their kind, tier and root. Claims nothing. Both take `kinds`, `tiers`, `repo` and `exclude_roots`; see [Choosing what to take](#choosing-what-to-take) |
| `work_extend` | Keeps the claim while the agent works (sent by `kbl-mcp` every 30 seconds) |
| `work_context` | Reads or adds context on an item |
| `work_link` | Adds a dependency between items in one tree |
| `work_post` | Creates a child under the item the agent holds: `split`, `augment`, `question` or `test`. Anything else is a proposal, except `prerequisite`, which is work another repo must do first |
| `work_rules` | Reads the standing rules in effect for the held item. Read-only |
| `work_progress` | Records a plan, a branch or a PR |
| `work_ask` / `work_answer` | Asks a person a question, which parks the item in Needs you and, from format 14, frees the agent, with optional `options` and a `default`. Answers one, with `chosen` or free text, while holding a `question` item under it. See [Questions with choices](#questions-with-choices) |
| `work_complete` / `work_release` | Finishes the item or hands it back, with the `usage` each model used (see [Cost and spend](#cost-and-spend)). `work_release` takes `why`: `failed` (the default), `waits_on_person` or `waits_on_outside`; see [Questions and waits free the agent](#questions-and-waits-free-the-agent) |
| `decide`, `record_step`, `edit` | Record a decision, a command it ran, or a code change, on the item ([decision-replay.md](decision-replay.md#what-gets-recorded)). A fix's decision can carry a revert check |
| `work_triage` | Answers the triage item the agent holds with a proposal for the item under triage: `tier` and `why`, an optional `duplicate_of`, `depends_on`, `related` and `split`, or `unclear`. Work the item needs from another repo is posted first, with `work_post`. See [Kinds of work](#kinds-of-work) |
| `work_finding`, `work_verdict` | Record a finding of the review the agent holds, and end a final review with a recommendation. See [Review as work](#review-as-work) |
| `supervise_start`, `supervise_stop`, `supervise_status` | Run a pool of worker agents from launch profiles, top it up on a timer, and show it. Only with `KBL_SUPERVISE_CONFIG`; see [supervise](design/supervise.md) |

Every `kbl-mcp` work tool, claiming included, calls the tracker over REST
with the agent's own token. The tracker checks the token, does the Felix work
itself and logs it in the same step, so an agent never touches the work
streams or keys. `work_next` polls the ready streams' consumer group as a
member named after the agent; `work_extend`, `work_complete` and
`work_release` extend, ack or nack that delivery. A claim is never held
without a `claimed` record, and an item is never acked before its `done`
record is written. An agent that dies stops extending, its claim lapses, and
the group hands the item to another agent.

The agent named on each record is the one the tracker authenticated. Felix
records the tracker as the publisher of every `work.events` record, so the
agent's identity is the tracker's attestation, written into the record.

An ingest worker pulls new and changed items from GitHub, GitLab, ADO or Jira. For each
root it posts one triage item and one augment item, once. Triaging again is a
person's action. People edit any record through REST or the viewer.

```mermaid
flowchart LR
    subgraph Sources
        GH[GitHub]
        GL[GitLab]
        ADO[Azure DevOps]
        JIRA[Jira]
        P[People, in the dashboard]
    end
    subgraph Agents
        A1[Agent + kbl-mcp]
        A2[Agent + kbl-mcp]
    end
    subgraph KBL
        T[Tracker]
    end
    subgraph Felix
        WE[(work.events)]
        RD[(work.ready.p0 to p3<br/>one consumer group)]
        KV[(item and budget keys)]
    end
    D[Dashboard]
    GH --> T
    GL --> T
    ADO --> T
    JIRA --> T
    P --> T
    A1 <--> T
    A2 <--> T
    T -- every change --> WE
    T -- ready items --> RD
    RD -- claims, as a member per agent --> T
    T --> KV
    WE -- fold --> D
```

Everything goes through the tracker: imports, people's actions and every
agent call. It is the only writer of `work.events`, the ready streams and the
keys, and the dashboard is a fold of `work.events`.

```mermaid
sequenceDiagram
    participant A as Agent (kbl-mcp)
    participant T as Tracker
    participant G as Ready streams (consumer group)
    participant L as work.events
    A->>T: work_next (agent token)
    T->>G: poll as member "agent"
    G-->>T: item, 90 s visibility
    T->>L: claimed
    T-->>A: item
    loop every 30 s while working
        A->>T: work_extend
        T->>G: extend
    end
    A->>T: work_complete
    T->>L: done
    T->>G: ack
    Note over A,G: An agent that stops extending loses the claim after 90 s and the group hands the item to another agent.
```

### Running the tracker

`kbl-tracker` signs in to Felix as its own principal, creates `work.events`,
`work.ready.p0` to `p3`, `work.review.r0` to `r3` and the consumer group on each if
they are not there, and folds `work.events` from the start before it
serves. It appends a record only when the fold accepts it, so a refused call
leaves no trace in the log.

**The fold to an offset.** `kbl_core::work_fold::fold_to(log, n)` is the state
after every record at an offset up to `n`: items, claims, decisions and refusals,
as the tracker held them when it had applied that record. Offsets that hold no
record are not in the list it is given. A Raft shard burns one when a
conditional publish is refused, so `n` at a blank is the state at the last
record before it. `Snapshots` keeps a copy of the state every 2,000 records
(records, not offsets) so a fold to a late offset starts near it. A snapshot is
only a cache: it is used after a hash over every record it covers, chained
from the previous snapshot's, is checked against the log, with its record count
and its fold revision against the build. An edit to any earlier record fails
it, and one that fails is skipped and rebuilt by `refresh`, so a missing or
stale one costs time and never changes the answer. A snapshot also keeps the
waiting-proposal counts, the human-wait record and the item fields that a
state's wire form leaves out, so a restored state answers budget and wait questions exactly as a
fold would. A test folds a log once, records the state at every offset,
and checks `fold_to` at each one, with and without snapshots, blanks included.

#### What Felix must guarantee

The tracker needs a record Felix acknowledged on `work.events` to stay at its
offset. Its checks look at where the log ends: a resume or an extend compares
the log's tip with the end of its state, and every append is conditional on
that offset. If a failover cut an acknowledged record and another writer
filled the offset again, the log would end where the tracker expects, and it
could hand an agent a claim that another agent now holds.

So `work.events` must be acknowledged only once a quorum holds it:

- a durable stream with `consistency: Quorum`, or one on Felix's per-shard
  Raft engine, where an ack is a Raft commit;
- or a durable stream with `replication_factor: 1`. With one copy there is no
  failover: Felix keeps the shard unplaced while its broker is down. This
  relies on the broker not losing an acknowledged record when it restarts, so
  run it with `FELIX_ACK_ON_COMMIT=true` and `FELIX_DURABLE_FSYNC_MODE=on_commit`
  (the dev stack does), and do not abandon the shard's log.

With one copy, the reliance also covers a record Felix never acknowledged:
it must not become readable and then vanish. That holds under fsync
`on_commit`, where a record is readable only once it is on disk.

At start the tracker reads `work.events` from the control plane and refuses
to start unless it is durable and either `Quorum` or one copy; a missing
replication factor is refused, not read as one copy. It reads the settings
again on every rebuild of its state, because a stream can be changed or
recreated while the tracker runs. If they no longer hold, the rebuild fails,
the state stays stale and every call is refused with an error that names the
settings, until the stream is fixed. The tracker keeps running rather than
exiting, so it serves again once the stream is fixed. The control plane does not show the shard engine or the
broker's ack and fsync settings
([felix#2735](https://github.com/GetFelix/felix/issues/2735)), so a stream on
Raft with more than one copy must still be `Quorum`, and fsync on a single copy
is up to whoever runs the broker.

The tracker also assumes a read of the log's tip shows every acknowledged
write. Felix does not promise that once reads need no leader lease: a deposed
`Quorum` leader can still serve a read that shows less
([felix#2735](https://github.com/GetFelix/felix/issues/2735) asks for a
read that reflects every acknowledged write). The cost is that a resumed or
extended claim can be reported live briefly after its hold ended. That is
wasted agent work and never two holders, because every claim is conditional on
the tail, so Felix refuses it and the tracker rebuilds.

The tracker does not defend against Felix losing a write it acknowledged. A
record Felix never acknowledged can still be cut, such as a publish whose
reply was lost; the tracker treats that as a lost reply and reads the log again.

#### Snapshots on disk

Set `KBL_TRACKER_SNAPSHOT_DIR` (or `--snapshot-dir`) and the tracker keeps
snapshots in `<dir>/<team>/snapshot-<records>.json`, so a restart folds only the
records since the newest one. The tracker still reads all of `work.events` and
hashes it, so the saving is the fold, not the read. A file holds the state, the
per-record data the tracker keeps beside it (claims, request keys, which edges
ingest made) and the chain links before it, with a digest of the body on its
first line. Message texts are not in it; a read of one that is not in memory
fetches it from the log, as after an eviction.

Every rebuild of the state tries the files newest first: the boot, and each
rebuild after the writer lease is retaken or goes stale. It uses the first that
passes the digest, the fold revision and the hash chain over the log, and logs
and skips each one that does not: damaged, cut short, from another revision, or
ahead of a log that lost its tail. A skipped file stays, so a log that was only
briefly short uses it again once it is back. A file the log contradicts
is removed once a rebuild has the log: each file carries the chain at every
earlier save, and one whose chain differs from the log at a point the log
covers was made from a log that was trimmed or started over, whatever its
record count. A file ahead of the log is kept, since the log may only be
short. Saving then keeps the last three of
what is left, and never removes the file it just wrote. With none usable it folds from the start. The folder is derived
data, so deleting it only costs a slower rebuild. A team folder that is a
symlink is not used. The disk work runs off the async workers.

`KBL_TRACKER_SNAPSHOT_EVERY` (2,000 records by default, `--snapshot-every`) sets
how often a snapshot is written, and 0 turns saving off. Smaller means a restart
folds less and the disk is written more; each save copies the whole state and
writes it, so it should stay well above the records a busy minute adds. A save
is temp file, `fsync`, rename, `fsync` of the folder, so a crash leaves the old
file. The last three are kept (the one just written always among them), and the
folder is mode 700. A failed write is logged and the tracker carries on.

Two things write snapshots. A rebuild (boot, lease retake, stale recovery)
writes one when it folded `every` records past the newest. The running tracker
writes one each time `every` records have landed since the last save, so a
tracker that never restarts keeps its snapshots current. Each landed record is
applied under the core lock and then, if a save is due, the state and the
per-record data are copied at that record, which is the one point where they
equal the fold of the log to its offset. The hashing and the file write run on
a blocking thread, so an append waits only for the copy, once per `every`
records. The hash chain is extended from the last save: the tracker keeps the
chain's head and the records since, not the log, so a save hashes only those
records. One save runs at a time per state, so one copy exists beside the live one (a
rebuild can briefly overlap the last save of the state it replaced). Each
rebuild starts a new generation for the folder, and a save taken before it
writes and prunes nothing, so it cannot remove a file the rebuild restored from. Records that land during a save wait for the next; if a stuck
disk lets more than the larger of 4 x `every` and 10,000 pile up, the tracker
logs it and stops saving until its next rebuild, which restarts from the log.
A snapshot is never trusted on how it was made: the next rebuild checks every
file against the log, and a file that does not match is skipped, so a
stale or foreign file costs a full fold. The check covers the records, not the
state inside the file, which is why the copy is taken at a record boundary. Run the tracker on a disk that survives
restarts, or the folder is empty each time.

Each record carries a format. A tracker that finds a record of a newer format
than it reads does not serve, instead of skipping the record.
[upgrades.md](upgrades.md) has the upgrade order, how to go back, and the
rules for changing a record.

Felix cannot yet tell the tracker which principal a token belongs to, so the
tracker reads a token map from a JSON file (`--tokens-file`, or
`KBL_TRACKER_TOKENS_FILE`). The file must be mode 600 or 400, a token may be listed
once only, and tokens are compared in constant time:

```json
{
  "agents": { "<token>": "w1" }, "people": { "<token>": "gabriel" }, "ingest": { "<token>": "ingest-gh" },
  "ingest_sources": { "ingest-gh": "github" },
  "grants": { "w1": ["github:GetFelix/felix-gateway"] }
}
```

`ingest_sources` is optional. It ties an ingest worker (by the name its token
maps to) to one source, `github` or `jira`. A tied worker imports and writes
only for that source's roots, with one exception: the `github` worker may
record the state of a pull request on a root from any source, since agents
open their requests on GitHub whatever tracker the issue is in. A worker with
no entry is not tied and reaches every root, which is only appropriate when
one worker runs, so the tracker refuses to start when some workers are tied
and another is not. A tied worker other than `github` lists only its own
source's roots. Tie each worker when a Jira worker and a GitHub worker run
together. The tracker refuses a key it does not know in this file, so a
misspelling fails at startup. A tracker from before `ingest_sources` ignores
it, so downgrading removes the ties.

`grants` is optional. It lists, per agent, the repositories it may file a
[prerequisite](#prerequisites-in-another-repo) in or link to, beyond the
repository of the tree it is working in. Each entry is `source:owner/repo`, so a
grant for `github:GetFelix/felix-gateway` does not reach a repository with the
same name in another source. The source is the name ingest records, such as
`github`, and is compared exactly. An agent with no grant can reach only its own
tree's repository.

The tracker listens on `127.0.0.1:8790` (`--listen`, or `KBL_TRACKER_LISTEN`)
and serves plain HTTP, so every bearer token crosses the socket in the clear.
It refuses to start on an address other than loopback unless you say a proxy
in front of it terminates TLS, with `--behind-tls-proxy` (or
`KBL_TRACKER_BEHIND_TLS_PROXY=true`). A proxy on the same host that forwards
to `127.0.0.1:8790` needs no flag; the flag is only for a tracker that must
listen on another address. Agents and ingest then call the proxy's `https://`
address. The callers do not check this: a `KBL_TRACKER_URL` or ingest
`tracker.url` that starts with `http://` and names another host sends the
token in the clear.

An agent's token maps to its name, which goes on every record it causes and is
the consumer-group member name. `kbl-mcp` reads `KBL_TRACKER_URL` and
`KBL_TRACKER_TOKEN`; without them the work tools say they are not set up.
Each session needs a token of its own; `kbl-tool agent add` makes one
([One agent token per session](#one-agent-token-per-session)).

| Call | Token | Does |
|---|---|---|
| `POST /v1/work/next` `{wait_seconds}` | agent | Polls `p0` to `p3` as the agent, writes `claimed`, answers with the item, its `attempt` and the `claimed` record's `offset`. An agent that already holds one gets it again, marked `resumed` |
| `POST /v1/work/extend` `{item}` | agent | Extends the delivery. `409` with `why: lost` means the claim lapsed; `503` means the tracker could not confirm its lease, so call again. After a `stale_claim` that check renews the lease, possibly twice and behind a renewal already running, each lease call giving up after 3 seconds, so with a slow Felix the `503` can take tens of seconds |
| `POST /v1/work/complete` `{item, claim?, attempt?, usage}` | agent | Writes `done`, then acks. Answers with the record's `offset`. A retry that lands while the tracker rebuilds after a lost reply gets `503`, so call again |
| `POST /v1/work/release` `{item, claim?, attempt?, reason, usage}` | agent | Writes `released`, then hands the delivery back. Answers with the record's `offset` |
| `POST /v1/work/give_up` `{item, claim?, attempt?, reason, usage}` | agent | Writes `gave_up`, then hands the delivery back, or acks it when the item is out of tries. Answers with the record's `offset` |
| `POST /v1/work/records` `{record}` | person | Appends a record a person may write, if the fold accepts it |
| `POST /v1/ingest/records` `{record}` | ingest | Appends an upstream sync record (`imported`, `accepted`, `context`, `linked`, `unlinked` of an edge ingest made, `updated`, `left_scope`, `returned_to_scope`, `reopened`, a `triage` or `augment` `proposed`, a `closed` as completed or upstream) as the tracker's role under ingest's name, if the fold accepts it. A `context` with the same text as the entry already holds, on an entry no agent wrote, logs nothing and answers `unchanged: true` with the entry's offset, so an ingest that lost its saved state and writes every open root's entries again does not grow the log |
| `GET /v1/ingest/roots` | ingest | Every imported root: item, source, upstream id, stage, whether it is closed, whether upstream sync (not a person) closed it (and whether that close was a merge) or last reopened it, whether upstream sync took it out of scope (`left_scope_by_sync`), whether it has children, whether an agent holds it, its pull request, when it was first claimed and its pull request status |
| `GET`, `PUT /v1/ingest/state/{source}` | ingest | Ingest's saved state (cursor and bookkeeping), kept in the `work.tracker` cache. A `PUT` names the version it read and is refused with `stale_state` if another save came first |
| `GET`, `PUT /v1/ingest/backoff/{pool}` | ingest | A budget pool's shared backoff, kept in the org's cache. `PUT {"wait_s": 120}` holds every worker of the pool off for that long (the later time wins, at most an hour); both answer `{"remaining_ms"}`. Refused with 403 unless one of the caller's team's connections spends the pool |
| `GET /v1/me` | person | The person's teams and role in each. A person in the token file is an org admin: `{"person": "gabriel", "org_admin": true, "teams": [{"team": "default", "role": "admin", "review_limit": 3}]}` |

A call that ends a claim (`complete`, `release`, `give_up`, and a
`prerequisite` post that ends it) can be retried safely when its answer was
lost, for example because the tracker died after writing the record. Send
`claim`, the `offset` from the claim's answer, as `kbl-mcp` does (it also
sends `attempt`). If that claim already ended with the same kind of record,
written by this agent (for a prerequisite, naming the same item), the tracker
writes nothing and answers as the first call did, with `replayed: true` and
the record's `offset`. A `claim` that is not the one the agent holds never
ends the claim it holds now, so a retry after the agent claimed the same item
again cannot end the new claim. Anything else is `lost` (or not held) and
writes nothing: a claim the tracker released as `lapsed`, another agent's
claim, or another kind of record.

Without `claim`, `attempt` does the same for the agent's latest claim of that
attempt. An attempt number is shared by every claim of a retried attempt, so
it cannot tell an old claim from a new one of the same attempt. Without
either, as from an older `kbl-mcp`, a retry is never replayed.

#### Request keys

Every other write an agent or a person makes can carry an optional
`request_key` (1 to 64 characters of letters, digits, `-` and `_`). If a call
may have landed and its answer was lost, send it again with the same key and
the same body: the tracker writes nothing and answers with the first write,
`{"item", "kind", "offset", "replayed": true}`. The reply names the first
record; it is not a copy of the first answer, which may have carried more.
Agent routes: `post` (not a `prerequisite`, which is retried by its claim and
refuses a `request_key`), `link`, `context/add`, `progress`, `decide`,
`finding`, `verdict`, `triage`, `step`, `edit`, `ask`, `answer`. Person routes:
`records`, `proposals/accept`, `proposals/edit`, `proposals/reject`,
`questions/answer`, `note`, `tier`, `close`, `unlink`, `budget`, `prices`,
`rules`, `flag`, `unflag`. The `kbl-mcp` tools for these take `request_key` too.

- **Where it is kept.** On the record the call wrote, as `req` beside `at_ms`
  and `by`, with a digest of the route and the body. A restarted tracker, or
  one that takes over, folds the log and finds the keys again. A reader that
  predates the field ignores it and folds as before, so replay of old logs does
  not change.
- **Upgrades.** `req` is a new optional field on the record. A reader that
  predates it ignores the field. During a rolling upgrade or downgrade, a
  tracker that predates request keys can hold the writer lease: it neither
  reads nor honors keys, so a keyed call it serves is written again on retry.
  Keep retrying under a key only once every tracker runs this version.
- **A caller that hangs up.** A write whose caller goes away while it is in
  flight may have landed. The tracker marks its state stale, so the next call
  rebuilds from the log, finds the key, and replays it.
- **Whose.** Each caller (an agent, or a person) has its own keys. Another
  caller's key is not a hit, and a person's key is not an agent's.
- **How long.** The last 256 keyed writes of each caller. A key older than that
  is forgotten and a call that reuses it writes again, so use a fresh key per
  call and keep retrying only while the answer is still in doubt.
- **A different body.** The same key with another body, or on another route, is
  `409` with `why: request_key_reused` and writes nothing.
- **Two at once.** A call whose key is still being written answers `503`; send
  it again and it gets the first write.
- **Refusals are not remembered.** Only a write that lands remembers its key, so
  a call the fold refused can be corrected and sent again under the same key.
- **The writer fence.** A replay writes nothing, so it takes no part in the
  `publish_if` fence. A write that does happen is as fenced as any other: if
  another tracker took over, the conditional write is refused, and the retry
  rebuilds from the log, finds the key if the other tracker wrote it, and
  answers with that record.
- **Not covered.** `work_next`, `extend` and the calls that end a claim (see
  above). The ingest routes need no key: they are idempotent by upstream id.

Every work and ingest route is also served under `/v1/teams/{team}/`, for
example `/v1/teams/default/work/next`. See [Which team a call is for](#teams).
A team takes at most `KBL_TEAM_INFLIGHT` calls at once (64 by default). A call
over that answers `503` with `why: team_busy` and `Retry-After: 1`. Waiting
`work_next` calls have a pool of the same size of their own, so agents waiting
for work never hold up a holder's `extend` or `complete`.

The other work tools are one route each, all with an agent's token. Every one
writes a single record attributed to that agent and answers with the record's
`offset`, which `decide` and `record_step` hand back so a later `edit` or
citation can name them. A refusal is a `409` with a `why` (`not_held`,
`over_budget`, `limit_reached`, `cycle`, `missing_field` and so on); a body that
does not parse, an unknown kind of work or text over the inline limit is a
`400` or `422` and tries nothing.

| Call | Does |
|---|---|
| `POST /v1/work/post` `{item, key, work, title}` | `work` is `split`, `augment`, `question` or `test` for a child under the held `item`, or `proposal` for out-of-scope work. The item's id comes from the key, so the same key again answers with the first item (`created: false`) and writes nothing. That answer carries `stage` and `closure` (null while open), so a key that hit a closed or rejected item (`stage: closed`, `closure: rejected`) is not mistaken for live work; the call does not reopen it, so use a new key |
| `POST /v1/work/post` `{item, key, work: "prerequisite", ...}` | Work another repo must do first. From a held triage item it is for the item under triage. See [Prerequisites in another repo](#prerequisites-in-another-repo) |
| `POST /v1/work/link` `{item, to}` | `item` waits on `to`. The agent must hold `item`, a child of it, or its parent, and both must be in one tree. The edge is refused with `cycle` if `to` already waits on `item`, directly or through other items |
| `POST /v1/work/rules/read` `{item}` | Reads the rules in effect for the held item |
| `POST /v1/work/context` `{item}` | Reads an item's context entries and the notes people left for the attempt |
| `POST /v1/work/context/add` `{item, entry, text}` | Writes a context entry on an item the agent holds, or on the parent of a child it holds. An agent replaces only entries it wrote itself: upstream text, the fold's entries and a person's notes stay as written |
| `POST /v1/work/progress` `{item, note, branch, pr}` | Records progress on the held item |
| `POST /v1/work/decide` | A decision: `phase`, `so_far`, `why` and `commit` are required; `evidence` is a list of `{stream, offset}` (see Evidence below) |
| `POST /v1/work/step` `{item, command, exit_code, output}` | A command the agent ran |
| `POST /v1/work/edit` `{item, file, hunk, decision}` | A code change and the offset of the decision it carries out |
| `POST /v1/work/ask` `{item, text, options?, default?}` / `POST /v1/work/answer` `{item, question, text?, chosen?, evidence}` | Asks a person, or answers as the holder of a `question` item. Below format 14 an answer starts resume work; see [Questions with choices](#questions-with-choices). From 14 the ask ends the claim and answers `claim: ended`; see [Questions and waits free the agent](#questions-and-waits-free-the-agent) |

**Evidence.** A decision, an answer and a new prerequisite can cite evidence: records that
show why, as `{stream, offset}`. A person reads them as support, so the
tracker holds them to the item's own tree. Each one must be a `work.events`
record that names an item with the same root, written before the record that
cites it. When another tree's record created the root, such as a deferred
finding or a prerequisite proposal, the tree may also cite that record, which
the root's context entry points at. A proposal is its own root, so the item
that filed it cannot cite records of the proposal's tree. Anything else is
refused: another tree's record answers `evidence_outside_tree`, another stream
`evidence_stream`, a later offset
`evidence_from_the_future`. A writer who may not make the record gets the
fold's refusal instead, so it learns nothing about where a record is. The
check runs before the append, so logs written before it fold as they did.

A redo (`forked_from`) of an item whose pull request has a failed check must
name a note: a `human_input` on the item, written after the `pr_status` that
reported the failure. Without one, or with an older note, the tracker answers
`note_required`. The note is what the next attempt starts from, and it shows
that someone read the failure before running the work again. A new pull
request on the item starts a new failure, and green checks end it. This check
also runs before the append, so it only sees a failure already reported: a
redo made while the checks are still running needs no note.

What happens to the redo next depends on the log's format
([upgrades](upgrades.md#formats)). Below format 4 the failed check keeps the
item in Needs you, so the redo waits until the checks are green. From format
4 the redo is handed out while the check is still red, as long as the item
has a note newer than the failure. If its claim is released or lapses, the
same attempt is handed out again until it is done. Each failure needs its own
read: if the checks fail again, for example after a re-run or after the redo
pushed, the item goes back to Needs you until a person writes a note on it
(any note on the item counts). That note is the way out; no new redo is
needed. Until then, or if no note is written, the redo waits for green.

Text in a record stays inline up to 4 KiB. Longer text is an artifact the
caller stored first and names by its hash; `kbl-mcp` does that for the
agent, and the tracker refuses long inline text.

**Secrets are cut out.** Tool output and diffs can carry a token the agent
read from its environment. Before any record an agent writes reaches the log,
the tracker replaces anything in its free text shaped like a known secret with
`[redacted]`: GitHub, GitLab, Slack, Google, npm and Stripe tokens, model
provider keys, AWS access key IDs, JWTs, `Bearer` and `Basic` credentials,
`token=` and `access_token=` values, the user and password in a URL, and a PEM
private key block. A person's note gets the same treatment, since people paste
tokens too. `kbl-mcp` does the same before it stores long text as an artifact,
a task's result, or a task's title or release reason. A secret with no
recognisable shape is not caught: an AWS secret access key is 40 characters
with no prefix, and so are many passwords. The match can also cut a name that
looks enough like a token, such as a branch `xoxb-2-RotateTokensV2x`. An
agent's grants should not include secrets it does not need.

The dashboard's actions are routes with a person's token. Each appends one
record if the fold accepts it:

| Call | Records |
|---|---|
| `POST /v1/work/proposals/accept` `{item, tier?}` | `accepted`. A proposal's tier comes from the edit, then this call, then triage |
| `POST /v1/work/proposals/edit` `{item, title?, tier?, depends_on?, split?, proposal?}` | `edited`, for an item nobody has accepted yet. `depends_on` and `split` replace the lists in triage's answer for the item and need `proposal`, the offset of the answer the person read (`proposal.at` in the item's row); see [Adjusting triage's advice](#adjusting-triages-advice). Needs format 17 |
| `POST /v1/work/proposals/reject` `{item}` | `rejected`, for an item nobody has accepted |
| `POST /v1/work/questions/answer` `{item, text?, chosen?, moves_to?, question?, evidence?}` | `answer`. The question defaults to the open one. `moves_to` is `{source, repo, title}` |
| `POST /v1/work/tier` `{item, tier}` | `tier_changed`. A ready item is published again on its new tier |
| `POST /v1/work/note` `{item, text, attempt?, input?}` (`text` is `{"text": ...}`) | `human_input`: a note, a review comment, an answer or a priority change, with the person who wrote it. `input` is `note` (the default), `review_comment`, `answer` or `priority_change`; it only labels the note, and a tier changes through `/v1/work/tier`. `attempt` ties it to one attempt; without it the note is on the item. A redo names one of these as its note. Known secret shapes in `text` are replaced with `[redacted]` |
| `POST /v1/work/close` `{item, why?}` | `closed`, with `merged`, `completed`, `dismissed` or `upstream` (default `dismissed`). Open descendants are cancelled |
| `POST /v1/work/unlink` `{item, to}` | `unlinked` |
| `POST /v1/work/relate` `{item, to}` / `POST /v1/work/unrelate` `{item, to}` | `related` / `unrelated`: a soft link between two items that holds neither back, shown on both. [Format 18](upgrades.md#formats); below it, refused with `format`. See [Related items](#related-items) |
| `POST /v1/work/budget` `{root, items_left, max_depth, cost_left_micros, redos_per_item}` | `budget_set`, the only way a budget goes up |
| `POST /v1/work/workers` `{worker, vendor?, profile?, model?}` (agent token) | `worker_started`. Registers a worker named `<your name>-<suffix>` that acts for you, and returns its token once. Capped by `KBL_MAX_WORKERS` per supervisor (default 8) and `KBL_MAX_TEAM_WORKERS` per team (default 32), refused with `worker_cap` past them. [Format 21](upgrades.md#formats); see [supervise](design/supervise.md) |
| `POST /v1/work/workers/stop` `{worker}` (agent or person token) | `worker_stopped`. The worker's token stops working; its hold goes back when its claim runs out. When a supervisor is revoked, removed or given another token, the tracker stops its workers and releases their holds at no cost to the items' tries |
| `POST /v1/work/workers/exchange` `{code}` (no token) | Trades the one-time code a start with `exchange: true` answered, instead of a token, for the worker's token. A code works once and for 10 minutes and is kept in memory only, so a tracker restart loses it. For hosted workers started through a hook; see [supervise](design/supervise.md) |
| `GET /v1/work/workers` (agent or person token) | Your workers, or every worker for a person: supervisor, vendor, profile, model, what each holds, and when it stopped |
| `POST /v1/work/prices` `{model, unit, price_micros, effective_from_ms?, expected_price?}` | `price_set`: the price of one unit of a model from now, or from a later time. See [Cost and spend](#cost-and-spend) |
| `POST /v1/work/rules` `{source, repo, root?, text?, checks?}` (`text` is a string) | `rules_set`, the [standing rules](#standing-rules) for a repo or one root |
| `POST /v1/work/flag` `{item}` | `flagged` with `flagged: true`: the item moves to Needs you and is not handed out. See [Flagging an item for a person](#flagging-an-item-for-a-person) |
| `POST /v1/work/unflag` `{item}` | `flagged` with `flagged: false`: the item leaves Needs you, unless it waits there for another reason |
| `POST /v1/work/retry` `{item}` | `retried`: an item that used its tries gets a fresh set. See [Card actions on My queue](#card-actions-on-my-queue) |
| `POST /v1/work/redo` `{item, from_attempt, at, note?}` | `forked_from`: a new attempt branches from the decision at offset `at` on `from_attempt`. `note` is the offset of a `human_input` from `/v1/work/note` that steers it. The fold refuses a closed item, one an agent holds, one with a redo already waiting, an unknown attempt or decision, two open attempts, and a spent `redos_per_item`; a red check on the open pull request needs a note written since (`note_required`). See [Redo from here](decision-replay.md#redo-from-here) |
| `POST /v1/work/snooze` `{item, until_ms}` | `snoozed`: My queue hides the item until `until_ms` |
| `POST /v1/work/block` `{item, reason, until_ms?, resource?}` | `block_set`: no agent is handed the item until `until_ms` or until a person lifts it. Needs `until_ms`, `resource` or both. See [Blocking an item](#blocking-an-item) |
| `POST /v1/work/lift-block` `{item}` | `block_cleared`: the item can be handed out again |
| `POST /v1/work/lift-resource` `{resource}` | `resource_lifted`: every item whose block still holds and names `resource` can be handed out again, in one record. See [Blocking an item](#blocking-an-item) |
| `POST /v1/work/park` `{item}` | `parked` with `parked: true`: My queue hides the item until it waits for a new reason |
| `POST /v1/work/unpark` `{item}` | `parked` with `parked: false`: the item is in My queue again |
| `POST /v1/work/unblock` `{item, to}` | `unblocked`: marks the dependency `to` resolved, so the item no longer waits on it |
| `POST /v1/work/request-review` `{item, reviewer}` | `review_requested`: records that the item's open pull request was put to `reviewer`. See [Review actions and summary marks](#review-actions-and-summary-marks) |
| `POST /v1/work/request-changes` `{item}` | `changes_requested`: records that a person sent the open pull request back |
| `POST /v1/work/mark-merged` `{item}` | `merge_marked`: records that the pull request was merged upstream, and closes the item as merged. Refused while upstream shows the request open; the item keeps who recorded it, and a later open or closed-unmerged status reopens it into Needs you as `merge_disputed` |
| `POST /v1/work/rate-summary` `{item, attempt, accurate}` | `summary_rated`: marks the summary the attempt handed off accurate or not |
| `POST /v1/work/take` `{item}` | `taken`: assigns the item to the caller. See [Assignees](#assignees) |
| `POST /v1/work/reassign` `{item, to}` | `reassigned`: assigns the item to `to`, `{"person": name}` of the caller's team (`not_member` otherwise), `{"agent": name}` of a team agent not revoked (format 13), or to nobody with `null` |
| `POST /v1/work/steer` `{item, attempt, text}` | `steered`: a note to the agent holding `attempt` |

A person's token can also read the team's work. These calls write nothing:

| Call | Answers with |
|---|---|
| `GET /v1/work/queue` | My queue: every item that waits on the caller, oldest wait first, then lower tier, as `{item, title, work, stage, needs, wait, tier, since_ms, root, assignee}`, with `count` (the whole queue) and `at`. `?limit=` trims `queue` to its first entries: 200 by default, at most 1000. `wait` is `proposal`, `question`, `failure`, `review` or `other`; an item whose reason changes starts a new wait from that record. An item a person snoozed or parked is left out while that wait continues (see [Card actions on My queue](#card-actions-on-my-queue)). `since_ms` is null for a wait the tracker did not see begin (it started before a snapshot), and those come first, since they are older than any known start. An item assigned to a person is only in that person's queue; anything else waiting is in everyone's. Person tokens only |
| `GET /v1/work/items` | Every item, in id order, as `{item, title, work, stage, needs, tier, parent, root, held_by, flagged, assignee, closed}`, and `at`, the log offset the answer reflects. `?stage=needs_you` (or any stage, such as `ready`, `held` or `blocked`) lists only that stage; a stage that does not exist is a `400` |
| `GET /v1/work/items/{item}` | The item as the fold holds it, with its `stage`, `needs` (why it waits on a person, or null) and `waits_on` (the items it waits for) |
| `GET /v1/work/agents` | The names of the team's agents an item can be assigned to, as `{agents}`: the token file's, and those added with `kbl-tool agent add` and not revoked. The dashboard's assign picker lists them |
| `GET /v1/work/items/{item}/history` | The records about the item, oldest first, each as `{offset, record}`: every record that names it, and those that point at it (a link to it, a prerequisite that created it or waits on it, a triage answer that calls it a duplicate). One answer holds at most 500 records and reads at most 20,000; `next` is the offset to pass as `?from=` for the rest, or null at the end of the log. A `from` past the end of the log is an empty answer with `next` null. A record the fold refused carries `rejected` with the reason, as of `at`, the log offset the tracker's state has reached |
| `GET /v1/work/metrics/time-to-human` | Time to first human action over the last `?days=` days (default 30, from 1 to 366), for the team: `stats.count`, `p50_ms` and `p90_ms` of the waits a person ended in the window, the same per reason in `by_kind` (`proposal`, `question`, `failure`, `review`, `other`), `open` (items waiting now with no person action yet) and `oldest_open_since_ms`. `truncated` is true, with `complete_since_ms`, when the oldest finished waits were dropped past the 20,000 kept, or the tracker restored its state from a snapshot, and the window reaches back before what the fold has seen. Open waits are not in the percentiles. See [Time to first human action](#time-to-first-human-action) |
| `GET /v1/work/metrics/summary-trust` | How often the summaries people marked were marked accurate over the last `?days=` days (default 30, from 1 to 366), for the team: `stats.rated` (attempts whose standing mark was written in the window), `accurate`, `not_accurate`, `accurate_permille` (null with nothing rated), `unrated` (finished attempts with no mark now, whatever the window), `min_sample` (10) and `undated`. A person only. See [Summary trust](#summary-trust) |

An unknown item answers `409` with `why: unknown_item`, like the agent calls.
A path that is not a valid item id is a `400`.
The other calls in this section follow the same rules as the agent tools: the
fold checks each record before it is appended, and a refusal is a `409` with a
`why`.

Ready records carry a stamp. When the tracker sees an item become Ready it
publishes a record, and a record that is not the newest for its item (a tier
changed, the tracker restarted, the item was closed or blocked meanwhile) is
acked and skipped when it is delivered. When Felix hands an item to a second
agent while the log still shows the first as holding it, the claim lapsed: the
tracker writes `released` with the reason `lapsed`, then the new `claimed`.
The log is the truth for holds. Before each call the tracker checks that the log
still shows the agent holding the item. If a person closed the item or the
attempt, or another agent took it, the agent's next `extend`, `complete` or
`release` answers `lost`.

Memory can miss a record another writer appended, for example a release a
broker failover wrote. So before it hands a held claim back to its agent (a
`work_next` that answers `resumed`, or an `extend`), the tracker reads the log
from the end of its state. If the log's end moved (a record landed, a new
leader's blank start, or a failover that cut the end short), it rebuilds from the log
and checks the hold again, as after a refused write: `work_next` then answers
from the rebuilt state, and `extend` answers 503 so the agent calls again and
hears `lost` if the hold ended. A new claim needs no read, since its `claimed`
record is written only if nothing landed after the state's end. The read sees
where the log ends, not what it holds, which is why the tracker needs
acknowledged records to stay put ([What Felix must guarantee](#what-felix-must-guarantee)).

When Felix answers an extend with `stale_claim` (it no longer has the
delivery), the tracker renews its lease by version before it answers `lost`.
Felix runs the lease out by its own clock, so a broker whose clock runs fast
can let another tracker take the lease and the claim while this one still
believes it holds them. If no renewal sent after the refusal is granted, or the grant retook a lease
that had run out, the extend answers 503
and the agent calls again, reaching whichever tracker holds the lease. This
assumes that over one lease, the clock of the broker holding the lease entry
runs ahead of the tracker's by less than the margin the tracker keeps before its
lease runs out, where it stops writing: 7.5 seconds at the default 15-second
lease.

The tracker settles a delivery in Felix only when it
still stands behind that exact delivery for that agent, because a group ack
names an offset, not a delivery: a stale holder's late call cannot ack the next
holder's.

A nack names an offset too. An agent that stalled past the visibility can
release at the moment Felix hands its record to the next poller, and its nack
then takes that poller's lease as well. So a delivery that was polled before the
last holder's hand-back ended is not turned into a claim. The record is free
again, or lapses, and the next poll claims it. Without this the new holder
would be claimed, lose its delivery at once and be lapsed while it worked.
The hand-back runs to its end even when the caller's request is dropped, and
no other call ends it early. If the task sending the nack panics, the hand-back
still ends, so the item can be claimed again once the old lease lapses and Felix
hands the record out.

Felix dead-letters a ready record after five handouts, and the tracker is not told. So a Ready item
whose record has stood for six visibility periods gets a new record with a new stamp; the old one,
if it is still in the lane, is acked when it is delivered.

A sweep (every few seconds) writes `released` with the reason `lapsed` for a
hold whose delivery the tracker no longer has, or that has not been extended
for the visibility period, so the item is Ready without another agent having to
poll.

After a restart the tracker takes back, for each agent the log shows holding,
the delivery Felix still has for that name. It polls the lane the item was
published on, where Felix hands a member its own deliveries before new work,
and stops at the first record that is the agent's item or another Ready item.
Other lanes are polled only when the item was not found there.

Each poll that hands out a record counts as a delivery attempt in Felix, and
Felix dead-letters a record after five. So:

- Each restart costs every held item one attempt (taking it back is a
  redelivery). A hold survives four restarts; the fifth dead-letters its record.
- The backlog of Ready records costs nothing when the held item is found.
- When an agent's delivery is gone, the first Ready record of each lane polled
  costs one attempt, once.

If Felix answers that the delivery is not there, the hold is released at once,
and until then `work_next` answers that the agent has no current claim.

If a poll fails instead, for example during a ready-lane failover, the hold is
kept. The tracker tries again in the background after each sweep, and on the
agent's next `work_next` or `extend`, skipping the lanes that already answered.
Those calls answer 502 while Felix still fails, so the agent calls again rather
than stopping work. A hold the tracker could not look for is freed one
visibility period after the first failure, when Felix has let the delivery lapse
as well.

The tracker is the only writer, and it holds that role with a lease: a key in
the `work.tracker` cache, written with a time to live and renewed every third
of it, starting as soon as the team serves. Renewal runs on its own timer and
keeps the lease apart from the item state, so neither a sweep nor an append
waiting on a slow Felix call delays it. Each lease put or read
gives up after 3 seconds, so one hung put still leaves two more renewals before
the lease runs out. Writes pause until the next renewal lands, because the
tracker stops writing when half the lease or less is left. A renewal that Felix
refuses while the lease entry cannot be read counts as a failed renewal, not a
lost lease: a slow Felix may have applied an earlier put of ours late. Teams start one after another, so a
team's lease can lapse (run out without a renewal) before it serves. The
tracker then renews its own entry if Felix still has it, and otherwise
takes it again, unless another tracker took it meanwhile. After taking it
again, the tracker rebuilds its state from the log before it writes, since the
other tracker may have written, and then renews the lease by version, so the
grant comes after the read. The same holds for the lease taken at start. A team whose tracker cannot renew answers 503
on its routes, stops renewing after ten seconds, and checks the lease before
each append. The process exits when no team holds its lease; with one team,
that is ten seconds after it lost it.

A lease put can land in Felix while its reply is lost, for example when it
times out on the tracker's side. The tracker then still has the old version,
and its next renewal by that version would be refused. So when a lease put is
refused or gets no answer, the tracker reads the lease entry. If the entry
names this tracker, it takes the entry's version as its own and renews by it.
Each tracker names itself with a random token drawn at start, so it never
mistakes another tracker's entry for its own. If its view of the lease had
already run out, the entry may come from a retake that landed after another
tracker held the lease, so the tracker treats it as a retake: it reads the log
again and confirms the lease by version after the read before it writes. If
the entry runs out between the read and the renewal, the renewal is refused
and the tracker takes the lease back with Absent, again as a retake. This holds
for team and org leases, and for the lease taken at start.

A tracker paused past its lease can wake up still believing it holds it, so
the lease is not enough on its own. Every append also names the offset it
expects to land at, the one after the last committed record this tracker read,
and Felix refuses it if anything else was given that offset first. After a
refusal, or a write whose answer was lost, the tracker reads the log again and
then writes nothing more until Felix renews its lease by version. The renewal
must come after the read, so no other tracker can have written in between.
Taking the lease again does not count: that grant comes before the read, so the
tracker reads and then renews by version. Until then no write goes out, even
from a call that started before the refusal. If
another tracker holds the lease, the grant fails and the tracker stops
writing. If the lease is still its own (a new shard leader moved the offset,
say), the write can go through. A refused team write answers 503 ("try
again"). An org write reads the log, checks the lease and retries by itself,
up to three times. Every write also needs the lease to stand for another
write's time limit, and an org write gives up with 503 once the lease it
started on runs that short, so it never keeps the renewal waiting. The next offset always comes
from a read of committed records, never from the refusal: the refusal's tail
counts offsets given to records that are not committed yet, and writing past
one would skip a record the tracker never read.

The tracker keeps no Felix call under its main lock, and every call has a
10 second limit, so one slow call stalls the agent that made it and nobody
else. If a write to `work.events` fails, the tracker rebuilds its state from
the log before it serves again, and refuses with 503 while it cannot.

**Connection cost.** The broker reclaims a member's deliveries only in a
connection's first poll, so each active agent has a QUIC connection of its own
to the broker, opened on its first call and closed after ten minutes without
one. A connection that fails is opened again with a fresh token on the next
call.

#### Flagging an item for a person

`POST /v1/work/flag` moves an open, accepted item to Needs you by hand, when
it needs something only a person can give before an agent works on it.
Agents do not get it from `work_next` until a person calls
`POST /v1/work/unflag`. Its reason in Needs you is `flagged`. It is the
opposite of parking on the dashboard, which takes an item out of your queue:
a flag puts it in, in front of every person in the team, and it is a record
on the log.

- If an agent holds the item, the agent keeps it. The item stops in Needs you
  once the attempt finishes or is released.
- If the item also waits for another reason (a question, a failed check, a
  budget), that reason is the one shown. Unflag clears only the flag; the
  other reason still needs its own answer.
- Flagging a flagged item, or unflagging one that is not flagged, is refused
  with `no_change`, so a call that moved nothing says so. So is flagging an
  item nobody has accepted: it waits for a person in Triage already. A closed
  item is refused with `closed`. Reopening an item clears the flag.
- A finished attempt on a flagged item shows in Needs you, not In review,
  until the flag is cleared.
- The dashboard has Flag for a person and Unflag in the item panel and the
  bulk bar, and palette commands for both. They send these routes with your
  token and ask no confirm, since each takes the other back; a refusal such
  as `no_change` shows with the tracker's reason.
- `flagged` is a record of [format 7](upgrades.md#formats). Until a person
  raises the log to 7, both calls are refused with `format`.

#### Blocking an item

An item can wait on something no agent can fix: an outside resource that is
unavailable (Azure credit at zero until a date), a person who is away, a
release freeze. `POST /v1/work/block` holds it back from every agent:

```bash
curl -X POST "$TRACKER/v1/work/block" -H "authorization: Bearer $PERSON_TOKEN" \
  -H 'content-type: application/json' \
  -d '{"item": "a1", "resource": "azure-vms", "until_ms": 1795305600000, "reason": "Azure credit is $0 until 11/21"}'
```

- `reason` is required and shown on the card and in the item panel.
  `resource` names what it waits on, such as `azure-vms` (letters, digits,
  `-`, `_` and `.`, up to 64). `until_ms` is when the block ends by itself:
  after the record and at most a year out. Give a time, a resource or both.
  With a resource and no time the block lasts until a person calls
  `POST /v1/work/lift-block`.
- `work_next` and `work_peek` skip a blocked item on every path: the lane, a
  filtered call, a call among several roots, and an item assigned to the
  caller. Nothing is written for it. The item keeps its stage (Ready) and its
  place; the board shows the block on the card.
- A time ends a block with no record and no background job. Whoever hands out
  work checks the clock, and the fold refuses a claim made before the time,
  so a restart gives the same answer.
- If an agent holds the item, the agent keeps it. Once the attempt is
  released it waits for the block like any other item.
- Setting a block that is already set, or lifting one that has ended or was
  never set, is refused with `no_change`. A block with no time and no
  resource, a time in the past or over a year out, an empty or over-long
  reason, or a resource name that is not plain is refused with `bad_block`.
  A closed item is refused with `closed`. Reopening an item drops its block.
- Only a person sets or lifts a block, with a person token. The tools are
  `block_item` and `lift_block`.
- `block_set` and `block_cleared` are records of [format 16](upgrades.md#formats).
  Until a person raises the log to 16, both calls are refused with `format`.
- Blocks are per item, and one call lifts them by resource.
  `POST /v1/work/lift-resource` with `{"resource": "azure-vms"}` (the tool is
  `lift_resource`) writes one `resource_lifted` record that ends every block
  that still holds and names exactly that resource, and nothing else: a block
  on another resource, a block with only a time, a block that already ended
  and a closed item are left as they were. The name is compared as written,
  so `Azure-VMs` is another resource. A call that would lift nothing is
  refused with `no_change`, an empty or over-long name with `bad_block`, and
  only a person may call it. It is a record of [format 20](upgrades.md#formats).
  Agents that say which resources they have: see
  [What the work needs](#what-the-work-needs-and-what-an-agent-can-do).

#### Related items

A relation links two items without ordering them. Unlike a dependency
(`linked`), it never holds either item back: both stay claimable whatever
state the other is in, and no stage, claim or `work_next` choice reads it.
Use it for "schedule these together", such as two items that should run on the
same machines.

- A person writes it with `POST /v1/work/relate` or the `relate_items` tool
  (`{item, to}`), and removes it with `/v1/work/unrelate` or `unrelate_items`.
  It goes both ways, so relating `a` to `b` is the same as relating `b` to `a`.
- It is shown on both cards as `Related: felix#376` and in the item panel under
  Related. The item read (`GET /v1/work/items/{item}`) lists `related`.
- An agent cannot write one. It proposes a relation by naming `related` in its
  `work_triage` answer, and a person decides, as for `depends_on`.
- An item has at most 20 relations (`limit_reached`), an item cannot be related
  to itself (`cycle`), and a loop of relations is fine because nothing follows
  them.
- It needs the log raised to [format 18](upgrades.md#formats).

`work_next` does not prefer an item related to the one an agent just held:
that would need a rule for when the preference gives way to tier order, and it
is left out until there is one.

#### Card actions on My queue

These are the person's moves on a card in [My queue](#my-queue). Each is one
record, written with a person's token for the team the token belongs to, and
each is [format 10](upgrades.md#formats): until a person raises the log to 10
the calls are refused with `format`. A repeat, or a move that changes nothing,
is refused with `no_change`; a closed item with `closed`.

- **Retry** gives an item that stopped for want of tries a fresh set on the
  attempt it was working. The item must be held by nobody, with an open
  attempt that is not done and has used its tries; for anything else the call
  is `no_change`. A failed check is not retried here: it takes a note or a
  redo, as before.
- **Snooze** hides an item from My queue until `until_ms`, which must be after
  now and at most 30 days away (`bad_time` otherwise). The item must wait on a
  person. **Park** hides it with no end time, and **unpark** shows it again.
  Both leave the item's stage alone, so an agent can still claim what is
  claimable, and neither counts as the person acting on the wait: the time to
  first human action keeps running.
- A snooze or a park covers the wait it was set on. If the item later waits for
  a new reason (an answered question that then fails, a proposal accepted and
  then stopped), the wait is new and the item is back in My queue. An item
  restored from a snapshot, whose wait began earlier, stays hidden.
- **Unblock** marks one dependency resolved: `to` must be an item the item
  still waits on, and the link is dropped, as `unlink` drops it. It differs
  in that the log says the person resolved it, and a link that holds nothing
  is `no_change`.

#### Person tools over MCP

`kbl-mcp person` serves the actions above and My queue as MCP tools, so a
person can work the queue from their own MCP host. It is a separate process
from the agent server, started with a person's token:

```bash
KBL_PERSON_TOKEN=... KBL_TRACKER_URL=http://127.0.0.1:8080 \
  kbl-mcp person --team default
```

| Tool | Does |
|---|---|
| `my_queue` | `GET /v1/work/queue`, with an optional `limit` |
| `accept_proposal`, `reject_proposal` | Accept a proposal, with an optional `tier`, or reject it |
| `answer_question` | Answer the open question on an item with `chosen` or `text` |
| `set_tier`, `flag_item`, `unflag_item` | Change an item's priority, or flag it |
| `add_note` | A note, review comment or priority change, as `human_input` |
| `redo_from_decision` | `POST /v1/work/redo`: start a new attempt from a recorded decision; `note` is an `add_note` offset |
| `retry_item`, `snooze_item`, `park_item`, `unpark_item`, `unblock_item` | The [card actions](#card-actions-on-my-queue) |
| `block_item`, `lift_block`, `lift_resource` | [Block an item](#blocking-an-item) until a time or on a named resource, lift one block, or lift every block on one resource |
| `classify_item` | Set an item's type, area, size or needs: [Labels, milestone, type, area and size](#labels-milestone-type-area-and-size) |
| `relate_items`, `unrelate_items` | Relate two items, or remove the relation: [Related items](#related-items) |
| `request_review`, `request_changes`, `mark_merged`, `rate_summary` | The [review actions and summary marks](#review-actions-and-summary-marks) |
| `take_item`, `reassign_item`, `steer_agent` | [Assignees](#assignees): take an item, assign it to a person (`to`) or to nobody (no `to`), or send a note to the agent holding it |
| `close_item` | Close an item as `dismissed` or `completed` |
| `edit_proposal` | Edit a proposal's title or tier, or replace triage's `depends_on` and `split` (these need `proposal`, the offset of the answer you read) |

Every action the dashboard offers on a card is a REST route and a person tool.
Each takes a person's token only; an agent's token gets `403`.

| Action | REST route | Person tool | Dashboard |
|---|---|---|---|
| My queue | `GET /v1/work/queue` | `my_queue` | Needs you |
| Accept | `POST /v1/work/proposals/accept` | `accept_proposal` | yes |
| Reject | `POST /v1/work/proposals/reject` | `reject_proposal` | yes |
| Edit a proposal (title, tier, dependencies, split) | `POST /v1/work/proposals/edit` | `edit_proposal` | dependencies and split |
| Answer a question | `POST /v1/work/questions/answer` | `answer_question` | yes |
| Set tier | `POST /v1/work/tier` | `set_tier` | yes |
| Flag, unflag | `POST /v1/work/flag`, `/unflag` | `flag_item`, `unflag_item` | yes |
| Note, review comment, priority change | `POST /v1/work/note` | `add_note` | no |
| Steer an agent | `POST /v1/work/steer` | `steer_agent` | yes |
| Rate a summary | `POST /v1/work/rate-summary` | `rate_summary` | yes |
| Take, reassign | `POST /v1/work/take`, `/reassign` | `take_item`, `reassign_item` | yes |
| Retry | `POST /v1/work/retry` | `retry_item` | no |
| Snooze | `POST /v1/work/snooze` | `snooze_item` | no |
| Park, unpark | `POST /v1/work/park`, `/unpark` | `park_item`, `unpark_item` | no |
| Block, lift | `POST /v1/work/block`, `/lift-block` | `block_item`, `lift_block` | no |
| Unblock a dependency | `POST /v1/work/unblock` | `unblock_item` | no |
| Relate, unrelate | `POST /v1/work/relate`, `/unrelate` | `relate_items`, `unrelate_items` | no |
| Request review, request changes, mark merged | `POST /v1/work/request-review`, `/request-changes`, `/mark-merged` | `request_review`, `request_changes`, `mark_merged` | no |
| Close | `POST /v1/work/close` | `close_item` | yes |

A column of `no` is an action the dashboard does not show yet; it is
reachable by REST and MCP. Not in the table: `POST /v1/work/unlink`, a dependency edit with no tool (use `unblock_item`), reopening a root, which only
upstream sync writes, and the org-level writes (budgets, prices, rules).

Every write takes an optional `request_key`, as the REST calls do. A tool
refuses a parameter it does not list before it calls the tracker.

The token is held by this process alone. It is read from `KBL_PERSON_TOKEN`,
which the agent server never reads, and at start the server asks the tracker
whom the token belongs to. An agent's token is not a person's, so the server
exits and lists no tools. The tracker also refuses an agent's token on every
person route, and writes each record under the person's name with the actor
`person`, so a record made through MCP looks like one made in the dashboard.
`--team` (or `KBL_TEAM`) fixes the team; a token in more than one team needs
it, and a token not in that team does not start. Run the person server in the
person's own MCP host, not in an agent's: an agent that can run a command in
the same environment can read the token.

Add a tool by adding an entry to the table in `mcp/src/person/table.rs`, and a
row to the threat model.

#### Assignees

An item has at most one assignee, as in Linear or GitHub. Any person of the
team may set or clear it, and it stays through a close and a reopen. The three
moves are [format 12](upgrades.md#formats): until a person raises the log to 12
they are refused with `format`. Each needs an open, accepted item (`closed`,
`not_ready`), and a move that changes nothing is `no_change`.

- **Take** assigns the item to you. **Reassign** assigns it to a person of the
  team, or to nobody. The tracker refuses a person outside the team as
  `not_member`.
- A person assignee means you are doing it yourself, so no agent claims it.
  Where the item would be Ready it waits in Needs you as `taken` instead. Other
  reasons to wait (a question, a failed check, used tries, a flag) come first:
  they say what to do, the assignee says who. An item in review or blocked
  stays there.
- If an agent holds the item, it keeps the hold. When it finishes or releases,
  the item stops in Needs you as `taken` instead of going back to Ready. Stop
  it (`released`) to end the agent's work now. Reassigning to nobody returns the
  item to Ready.
- **Steer** sends a note to the agent holding the item: it names the attempt
  held and is refused without a holder (`not_held`) or for another attempt
  (`wrong_attempt`). The agent reads it as a note on its attempt, through the
  same read as any other note, and it counts as a note for redoing a red check.
- My queue is per person: an item assigned to a person is only in that
  person's queue. Unassigned items are in everyone's.
- A take, a reassign and a steer each count as a person acting on the wait for
  [time to first human action](#time-to-first-human-action). The wait a take
  starts is counted as acted on, since the person chose it.
- **An agent assignee** (`{"agent": "w1"}`, from [format 13](upgrades.md#formats))
  means only that agent works the item. It must be an agent of the team that
  is not revoked (`not_member`), and only a person sets it. The item stays
  Ready and shows in everyone's My queue when it waits on a person. Below
  format 13 it is refused as `bad_assignee`.

##### How the claim reaches the assigned agent

The fold refuses `claimed` by any other agent as `not_assignee`. A hold is
still one slot that a claim fills only from Ready, so assigning or reassigning
while an agent holds the item never makes a second holder: the holder keeps
it until it finishes, releases or lapses, and only then can the assignee claim.

The tracker does not publish an agent's item to a ready lane. When the
assignee calls `work_next`, the tracker claims its assigned Ready item in the highest-priority lane
straight from the state, ahead of the lanes, and nobody else is ever handed
it. A ready record published before the assignment is acked by whichever
agent polls it and is not published again, so:

- no agent gets the item back, and none passes it between them;
- the item spends none of Felix's handouts, which dead-letter a record at
  their limit: the record is acked, never nacked;
- the assignee is never starved, since nothing else can claim the item.

A direct claim has no Felix delivery. The agent extends it as usual and the
tracker's sweep lapses it when it is not extended.

A restarted tracker takes a direct claim back from the log without polling
Felix, whoever the item is assigned to by then. A `claimed` record from format
15 says whether it was direct (`direct: true`), which covers a claim made for a
filter or among several roots, where the item is assigned to nobody. A claim
from before then has no flag and counts as direct only when the item was
assigned to the claiming agent, since a lane claim is never written for an item
assigned to an agent. Until the format is raised the tracker writes no flag, so
an unassigned direct claim does not survive a restart. A lane poll that meets
the held item's old record before its agent has called again does not lapse the
hold either. The hold then runs one visibility period from the restart, past any
deadline the old tracker gave, and lapses unless the agent extends it. So a
restart neither ends a direct claim early, which would let a new holder start a
second attempt while the first still works, nor spends a handout on another
item's ready record looking for it.

If the assigned agent goes away (it is revoked, or makes no call to the
tracker to claim, extend or finish work for `KBL_ASSIGNEE_AWAY_SECS`, 600 by
default, counted from the tracker's start), the sweep writes
`reassigned` to nobody as the tracker, and the item goes to the whole team.
The story shows why. A busy agent working another item still extends it, so
it keeps its assigned items. A revoke drops the assignee at once only on the
tracker that served it; a restarted tracker waits out the away time.

Reassigning or clearing an agent while it holds the item leaves its hold in
place until it finishes, releases or lapses, across a tracker restart too.

#### Review actions and summary marks

A person's moves on an item's pull request, and on the summary an agent handed
off. Each is one record from a person's token, [format 11](upgrades.md#formats):
until a person raises the log to 11 the calls are refused with `format`. A move
that changes nothing is `no_change`.

`pr_status` is what upstream says and only ingest writes it. These records never
change it. They are what a person did, kept beside it on the item.

- **Request review** names who was asked (`reviewer`, up to 100 characters, no
  control characters). The pull request must be open upstream, reported by the
  item, and not already approved; a repeat of the same ask is `no_change`, and
  another reviewer replaces it. The tracker does not contact anyone: the ask is
  a note on the item, so it is for a person to make it on GitHub as well.
- **Request changes** records that a person sent the open pull request back.
  Same conditions as the ask, and it answers a standing ask. It does not start a
  redo: that is still a note or a redo, as before.
- Both go stale rather than wrong. Upstream showing a review (approved, or
  changes requested for an ask), the request no longer open, or the item
  reporting a different request drops them, so a later round starts clean.
  A new ask also drops the earlier send-back.
- **Mark merged** is for a merge the person saw that ingest has not recorded. It
  works when upstream has not reported the request, still shows it open, or
  already shows it merged, and closes the item as merged, as a `close` with
  `merged` does. It is refused as `no_change` when upstream shows the request
  closed without a merge, or the item reports no request. The tracker never
  merges.
- **Rate summary** marks the summary one attempt handed off: `attempt` must
  exist and be finished (`wrong_attempt`, `not_ready`). The latest mark for an
  attempt stands, and the same mark again is `no_change`. It works on a closed
  item too, since it judges what the agent wrote. It is not an action on the
  wait, so time to first human action keeps running. The mark shows in the
  story section of the panel; nothing computes a trust number from it yet.

### One agent token per session

Two `kbl-mcp` sessions that share a token claim as one agent, so give each
session its own. With teams on (`KBL_ORG_NAMESPACE=kbl`), an org admin adds
an agent with one command, and its token works on the next call, with no
restart:

```bash
export KBL_TRACKER_TOKEN=<an org admin's token from the token file>
kbl-tool agent add w2 > w2.token      # prints the token once; keep it like a password
kbl-tool agent add w3 > w3.token
kbl-tool agent add w4 > w4.token
kbl-tool agent list                   # each agent and what it holds
kbl-tool agent revoke w3              # its next call gets 401; its holds are released
```

Start each `kbl-mcp` session with one of those tokens as its
`KBL_TRACKER_TOKEN`. The agent works in `KBL_NAMESPACE`'s team unless
`--team` names another.

- The tracker makes the token from 32 random bytes and answers it once. Only
  its SHA-256 is kept, on `org.events` as `agent_added`, so a lost token
  cannot be shown again: revoke the agent and add another.
- A name is used once per team. A name already in the token file, or ever
  added before, revoked or not, is refused with `name_taken`, so a new token
  never takes over an old agent's holds or records.
- Revoking writes `agent_revoked` and the token stops working. Every item the
  agent holds is then released with the reason `revoked`, the way the sweep
  releases a lapsed claim, and the item is ready for another agent.
  Running the revoke again writes nothing and releases anything still held.
- Agents in the token file keep working as before. They cannot be revoked
  with the command: remove them from the file and restart the tracker. The
  tracker will not start while the token file names an agent `agent add`
  minted and nobody revoked.
- A team has at most 256 live agents. Revoking one frees its place.
- While the tracker does not hold the org lease, a token the token file does
  not name gets `503` with `org_unavailable`: another tracker may have revoked
  it. It serves again once it has the lease back and has reread `org.events`.
- Without teams on, the agent routes answer `404` with `teams_off`.

### Running the ingest worker

To set ingest up for the first time, start with [Setting up ingest](ingest-setup.md): `kbl-tool source init` writes a config and `kbl-ingest check` tests it, writing nothing.

`kbl-ingest` is a separate binary. It never writes to Felix: it reads
GitHub, then writes through the three `ingest` calls above under its own token,
and it makes no model calls. `kbl-ingest --config ingest.json` polls every
`poll_seconds` (default 300, a minimum of 60: the gap stretches when the request
budget needs it; see [Request budget](ingest-setup.md#request-budget));
`--once` polls once. `--webhook-listen` adds an
endpoint whose only effect is to wake the poll (a `check_suite`,
`pull_request` or `pull_request_review` event makes the next poll read the
pull request states at once): it checks `X-Hub-Signature-256`
against the secret named by `--webhook-secret-env`, and a `transferred` or
`deleted` event also forces a reconcile. An empty secret is refused, a repeated
`X-GitHub-Delivery` id is dropped, and wakes are at least
`--webhook-min-gap-seconds` apart (default 10). The endpoint serves plain
HTTP and a delivery carries the issue's text, so, like the tracker, it listens
only on loopback unless `--webhook-behind-tls-proxy`
(`KBL_INGEST_WEBHOOK_BEHIND_TLS_PROXY=true`) says a TLS proxy is in front.
GitHub reaches it through that proxy, which can forward to loopback without
the flag.

The ingest token can act only on roots ingest imported. A pull request an agent
reports is written to (made a draft) only when it is in the same repository as
the upstream issue, and that repository is one a rule names. A root a person
dismissed stays closed when its issue is reopened. Whether a close was
ingest's is read from the log (`GET /v1/ingest/roots`), not from saved state, so
a poll that stopped between the close and its save does not lose it. The same
goes for a reopen: an open root whose issue is closed stays open only when a
person reopened it. When ingest imported or reopened it and the issue closed
before a save landed, the next poll closes the root.
A close whose save was lost, with the issue open again by the next poll, is
undone the same way, unless the close was the merge of the item's pull request
(`closed_by_merge`). An imported root is saved as tracked only after its context
and admission are written, so a poll that fails partway resumes them.

Ingest's saved state keeps an entry for each open issue, each transferred
issue, and each closed issue still waiting on its pull request. An issue that is closed
and settled (its root is closed, and a dropped issue's pull request has been
dealt with) is forgotten, so the state does not grow toward the 4 MiB limit.
Reopening such an issue still reopens its root, because the root says whether
ingest closed it.

```json
{
  "tracker": { "url": "http://127.0.0.1:8790", "token_env": "KBL_INGEST_TOKEN" },
  "github": { "token_file": "/run/secrets/github-token", "own_logins": ["kbl[bot]"] },
  "rules": [
    {
      "name": "agent-ok", "repo": "GetFelix/felix", "mode": "admit",
      "labels_all": ["agent-ok"], "labels_excluded": ["wontfix"],
      "priority_labels": { "p0": 0, "p1": 1 }, "default_tier": 2, "cap": 5
    }
  ]
}
```

A rule's `repo`, `labels_all`, `labels_any`, `labels_excluded`, `milestone`,
`assignee` and `type` are ANDed; `mode` is `admit` or `propose`; `triage` is `run` (the default) or `skip`, which with `admit` imports a root with no triage or augment item; `cap` limits
open roots; `target_repo` names where agents work (default: the issue's repo).
The first rule that matches takes the issue.

The GitHub credential is one of three, and the first one set is used:

1. `token_file`: a file holding a token, read again for every request, so a
   token refreshed beside the worker is picked up.
2. `app`: ingest mints installation tokens itself.
3. `token_env`: the environment variable holding a token.

```json
"github": {
  "app": { "id": 123456, "installation_id": 7890123, "private_key_file": "/run/secrets/kbl-app.pem" },
  "own_logins": ["kbl[bot]"]
}
```

`app.id` is the App's id (or client id). Name the installation with exactly one
of `installation_id`, `installation_repo` (`owner/name`) or `installation_org`;
the last two ask GitHub which installation serves that repo or organization,
and remember the answer. If GitHub then refuses that installation (404 or 401)
because the App was uninstalled and installed again, ingest asks once more and
fails with an error if the new answer is refused too; a fixed `installation_id`
is never looked up again. `private_key_file` must be mode 600 or 400, like the tracker's token file;
ingest refuses to start otherwise. A GitHub `api` that is not https logs a
warning, and so does setting more than one credential (the first in the list
above wins). `private_key_file` is the PEM GitHub gives out when the key is generated
(`RSA PRIVATE KEY` or `PRIVATE KEY`). Ingest signs a JWT with it (RS256, issued
60 seconds in the past, valid under ten minutes), trades it at
`POST /app/installations/{id}/access_tokens` and keeps the token until five
minutes of its hour are left, and reuses a token minted in the last minute
whatever the local clock says, so a fast clock does not mint on every request. A request GitHub answers with 401 mints a new
token and is sent once more; a second 401 stops the request with an error that
names the App id, never the credential. The key, the JWT and the token are never
logged, and an error from GitHub is passed through the secret redactor first.
The token stays in memory.

`open_prerequisites: true` on a rule lets ingest open issues in that rule's
`repo` for prerequisites, as in
[Prerequisites become upstream issues](#prerequisites-become-upstream-issues).
The credential then needs issues: write on that repo, on top of the read
access every rule needs. Leave it off and ingest sends no such request.

What it does with GitHub:

- **Cursor.** Each repo has its own cursor. Issues and comments are listed
  `since` that repo's cursor, oldest first. Re-listing is safe, because each
  step is idempotent: an issue listed again, or listed after ingest lost its
  saved state, is imported once (`dev/check-tracker-ingest.mjs` runs this
  against the dev stack). A repo that answers 403 or 404 (removed, renamed, or the
  App lost access) keeps its cursor and stalls only itself: the tracker gets a
  `source_stalled` record, shown in Needs you for that source, and a second
  record when the repo can be read again. State saved with the older single
  cursor is read once: every repo starts from it. The saved state also keeps a
  digest of each rule's selector (repo or project, labels, milestone, assignee,
  type, query). A rule that is new or whose selector changed drops its repo's
  cursor, so the next poll reads that repo in full, once, within the rule's cap;
  repos whose rules did not change keep their cursors. State saved before the
  digests existed reads as unchanged, so an upgrade re-reads nothing. This holds
  for GitHub, Jira, Azure DevOps and GitLab, which all keep one cursor per repo
  or project. A single-page
  listing is requested with `If-None-Match`, so an unchanged repo costs no rate
  limit. A spent budget or `retry-after` makes every later call wait.
- **Import once.** The item id is derived from the upstream id, and the fold
  refuses a second `imported` for it. A poll that stops before it saves finds
  its root again on the next poll and adds nothing.
- **Admit** writes `imported`, context for the issue text and the rule,
  `accepted` with the tier, and a triage and an augment item. With `"triage": "skip"` it writes the `accepted` (carrying the rule's name as `triage_skipped`) and no items, so the root is Ready to fix at once. **Propose**
  writes `imported` and the context, and leaves the root in Triage.
- **Cap.** A match over its rule's cap is not imported and is looked at again
  on later polls, oldest first, as roots close.
- **Comments** become `context` entries (`gh-c<id>`) that name the author.
  Comments by ingest's own logins are skipped, and an edited comment replaces
  its entry. Comments made while an issue waited over its rule's cap are read
  from the issue when it is admitted.
- **Transfers and deletions** show in no listing, so each open imported issue
  is fetched once every `reconcile_hours` (default 24). Each poll fetches its
  share, about `poll_seconds / (reconcile_hours * 3600)` of them, in key order
  after the last one read, and a webhook that names a transfer or deletion
  fetches everything at once. A redirect is a move
  (`context` entry `moved`, new id, same item); a missing issue is dropped.
- **Dropped with an open pull request.** After `closed` (dropped), the worker
  makes the pull request in the item's `pr` a draft and comments on it, once.
  It is the only write to GitHub.
- **Pull request state.** Each poll reads the pull request in an open root's
  `pr`, and writes a `pr_status` record when its phase, reviews or checks
  changed. The agent names the request, so it is read only if all of these
  hold: it is in the issue's repo and a rule names that repo; one of
  `own_logins` opened it; it was created after the item's first claim; and it
  is tied to the item, either by a closing reference to the issue in its body
  (`Fixes #812`) or by its head branch, which is `kbl/<item id>`. The claim
  reply gives the agent that name as `item.pr_branch`. The branch the agent
  reported in `work_progress` is not a link, and the tracker refuses a `pr`
  that another open item already reports (`pr_in_use`), in any letter case:
  `getfelix/felix#12` and `GetFelix/felix#12` are one request, and the
  tracker records the casing already in use.
  Reviews count only from the repo's owners, members and collaborators, the
  latest per reviewer; a dismissed review no longer counts. Every page of
  reviews is read. If GitHub refuses a page, or there are 2,000 reviews or more, the
  request reads as not reviewed (or as waiting on the reviewers asked), never
  as approved from part of the list. Checks are the
  check runs and the commit statuses on the head commit, with statuses paged
  so none are lost; a status written by one of `own_logins`, or a check run
  written by an App whose `<slug>[bot]` login is in `own_logins`, is ignored
  (so a required check must come from another App, such as GitHub Actions). When
  the credential may read the base branch's protection, only the required
  checks count, and a required one with no result yet is pending; an empty
  list means unknown and every check counts. A listing that failed or was cut
  short is pending, never green and never "no checks"; a red result is kept.
  A merge counts only into the repo's default branch and by someone other
  than `own_logins` (agents never merge their own work, a person does), and
  then closes the root as merged (`closed` with reason merged, which the
  ingest token may write only for a merged status of the item's own request).
- **Releases.** A merged root that an open item's link waits on for a release
  (`awaits_release` in `GET /v1/ingest/roots`) gets a `shipped` record once a
  published release carries its merge commit. The worker records the oldest
  stable release after the merge that contains it: drafts and prereleases are
  ignored. It lists the repo's releases newest first, at most 5 pages of 100.
  GitHub orders that list by creation time, so a draft published long after it
  was made can sit further down; the worker reads one more page past the first
  release published before the merge to catch those, then stops. If the 5
  pages run out before it sees a release older than the merge, it records
  nothing for that root, never a release that may be later than the right one,
  and says so in its log: a person records `shipped` for it. It then compares
  the merge commit with each tag, oldest first (`ahead` or `identical` means
  the tag contains it). One poll makes at most `release_compares_per_poll`
  requests in this step, pull request reads and compares together (default
  20), and a tag found not to contain the merge is not compared again, so the
  search carries on over later polls. A request GitHub answers 404 or 410 for
  is read again on a later poll. A root whose compare fails, whose `shipped`
  is refused, or whose merge is past the page bound sits out the next 10 polls,
  so it cannot spend every poll's budget. This step runs last in a poll, after
  the cursors move, so a rate limit here holds up nothing else and the tags
  already found not to contain the merge are kept. A request merged without a merge time, or by one
  of `own_logins`, ships nothing. It checks only a request it may read under
  the rules above. The token needs read access to the repo's contents for the
  compare call and the release list. A merged root nothing waits on costs no
  requests. A tag that is later moved or deleted is not looked at again, and a
  fix that reaches a release only by a cherry-pick is never found: a person
  records `shipped` for those.
- **Links.** Blocked-by and sub-issues become `linked` edges once both issues
  are imported, in the same repo or another one the rules import. A link to
  an issue no rule imports waits until one does. When a dependency is removed
  upstream, ingest writes `unlinked`, and an item that waited only on it
  leaves Blocked. Ingest removes only an edge its own `linked` created. It
  never removes an edge that was there before its link: one from an agent's
  prerequisite (with its release wait), from an answer that moves the work
  elsewhere, or one a person or an agent linked. The tracker refuses ingest's
  `linked` of such an edge as `already_linked`, and its `unlinked` as
  `not_ingests`. A person who links an edge ingest made makes it theirs too,
  and so does an agent's prerequisite that names it. Ingest does not retry a
  link refused as `already_linked` until upstream drops the dependency or the
  next reconcile.
  An agent's `work_link` of an edge that is already there is refused as
  `already_linked` and not logged, so the edge stays ingest's and goes when
  upstream drops it; the agent can link it again after that. Nothing is
  unlinked from a listing that may be incomplete: a failed or rate-limited
  read, a list with a second page, a redirect or 410 for a transferred issue,
  404 on both lists, or an entry without a number. A 404 on one list while
  the other answers means that list is not offered for the repo, and it
  counts as empty. A dependency that was transferred keeps its edge: it is
  listed under its new id, and if ingest has not seen the move yet it reads
  the old id first and leaves a redirected one alone. One in a repo ingest
  cannot read at the moment is left alone, since GitHub may just be hiding it.

An issue that leaves its rule and later matches it again gets a
`returned_to_scope` record, and the item leaves Needs you for the stage it had.
A person can write the same record to keep an item whose issue no longer
matches. `returned_to_scope` is the first record of format 2, so the tracker
refuses it until a person raises the log to format 2 (see
[upgrades](upgrades.md#upgrade)). Until then a returning item stays in Needs
you, as before, and ingest writes the record at its next reconcile after the
raise, or sooner if the issue changes. Ingest brings back only items it took
out: a `left_scope` a person wrote stays until a person clears it.

An issue is held to the rule that imported it. Renaming or removing that rule
in the config takes its items out of scope. One case has no rule: an issue
reopened after ingest forgot it, while no rule matched it. The first rule that
matches it later becomes its rule, if that rule is under its cap.

### Grants

| Principal | May do |
|---|---|
| The tracker | The only publisher of `work.events` and `work.ready.*`, and the only writer of item and budget keys |
| An agent | Call the tracker with its own token, for claims and everything else. Read items and the stream. No access to `work.ready.*` |
| A person | Use the dashboard and REST. Their actions reach `work.events` through the tracker |
| The viewer | Read only |

With [teams](#teams), each row holds per team: the tracker publishes to a team's streams only through a Felix token narrowed to that team's namespace, and a viewer reads only the teams they belong to.

### Teams

Partly built: the `org.events` records and fold exist in `kbl-core`. The tracker keeps a team table and serves the `/v1/teams/{team}/` paths and `GET /v1/me`. With `KBL_ORG_NAMESPACE` set it reads `org.events`, and `POST /v1/org/teams` (with `kbl-tool team create`) adds a team while it runs. Agents can be added and revoked on `org.events` ([One agent token per session](#one-agent-token-per-session)). Members, sources, ingest tokens and grants are not on `org.events` yet: the token file's people are the org admins, and its agents and ingest workers belong to `KBL_NAMESPACE`. This is the sketch for [Teams in design.md](design.md#teams)
([#185](https://github.com/GetFelix/KBL/issues/185)). One tracker serves
several teams, each in its own Felix namespace with its own `work.events`.

**Settings.** One new setting turns teams on. Without it the tracker runs as
it does today.

| Variable | Default | What |
|---|---|---|
| `KBL_ORG_NAMESPACE` | unset | The namespace that holds `org.events`, and it must be `kbl`. Set, the tracker serves every team the org log lists. Built |
| `KBL_NAMESPACE` | `default` | The first team. With teams on, it is adopted as a team the first time the tracker finds `org.events` empty |
| `KBL_TRACKER_TOKENS_FILE` | required today, optional with teams | Read as today. Its entries belong to the first team, and its people are org admins |
| `KBL_TEAM_INFLIGHT` | 64 | Calls in flight per team, so one busy team cannot starve the others. Built |

Teams, people, roles, agents, sources, ingest workers and grants come from
`org.events`, not from a file, so they change without a restart. Each holds a
token's SHA-256, never the token. A record on `org.events` looks like this:

```json
{"kind": "agent_added", "team": "payments", "agent": "w1",
 "token_sha256": "<hex>", "by": "ana", "at_ms": 1762900000000}
```

**Which team a call is for.** An agent or ingest token is bound to one team,
so its calls go to that team and `kbl-mcp` and `kbl-ingest` need no change.
A person can be in several teams, so person calls name the team in the path:

| Path | Team |
|---|---|
| `/v1/teams/{team}/work/...` | `{team}`. Every route under `/v1/work/` and `/v1/ingest/` has this form too |
| `/v1/work/...`, `/v1/ingest/...` | The token's team. A person in more than one team gets `400` with `why: team_required` |

A team the caller does not belong to answers `404` with `why: unknown_team`,
the same as a team that does not exist. An agent or ingest token used with
another team's path gets the same answer.

**Org and team routes.** All take a person token. The tracker checks the
role, appends to `org.events` if the org fold accepts the record, and answers
with the record's offset.

| Call | Role | Records |
|---|---|---|
| `GET /v1/me` | Any person | Nothing. Answers the person's teams and role in each |
| `GET /v1/org/teams` | `org_admin` | Nothing. Every team, whether it was adopted, and whether it is serving. Also `org_writable`, see below. Built |
| `POST /v1/org/teams` `{team, display_name?}` | `org_admin` | `team_created`. The tracker then creates the team's streams and group if they are missing, takes its lease and serves it. Answers `{team, created, offset, serving}`; a team already on the log answers `created: false`. Built |
| `POST /v1/teams/{team}/members` `{person, role, repos?, subject?}` | Team `admin` | `person_added` for a new person, with a new token answered once; then `role_granted`. `repos` narrows the role to some of the team's repos |
| `DELETE /v1/teams/{team}/members/{person}` | Team `admin` | `role_revoked` |
| `POST /v1/teams/{team}/agents` `{agent}` | `org_admin` today; team `triager` once roles exist | `agent_added`, with a new token answered once: `{team, agent, token, offset}`. Built |
| `GET /v1/teams/{team}/agents` | `org_admin` today; team `viewer` once roles exist | Nothing. Names, who added each, whether each is revoked, and what each holds. Token-file agents are listed with `token_file: true`; one whose name was revoked while minted carries `revoked_minted: true` instead of a second row. Built |
| `DELETE /v1/teams/{team}/agents/{agent}` | `org_admin` today; whoever added it, or team `admin`, once roles exist | `agent_revoked`, then a `released` with the reason `revoked` on the team's log for each hold. Answers `{revoked, released, offset}`. Built |
| `POST /v1/teams/{team}/sources` `{source, repo}` | Team `admin` | `source_added`. `409` with `why: source_owned` when another team owns it |
| `POST /v1/teams/{team}/ingest` `{name}` | Team `admin` | `ingest_added`, with a new token answered once |
| `POST /v1/teams/{team}/grants` `{from_team, agents?, repos?, max_pending?}` | Team `admin` of the receiving team | `grant_added`: agents of `from_team` may file prerequisites here |
| `DELETE /v1/teams/{team}/grants/{from_team}` | Team `admin` | `grant_removed`. Proposals already filed stay |

`org_writable` on `GET /v1/org/teams` says whether this tracker holds the org writer lease and has read `org.events` since taking it. While it is `false`, a team create or an agent add or revoke will most likely answer `503` with `org_unavailable`, so a script that starts the tracker waits for `true` first, as `dev/check-teams.mjs` does.

`org_admin` passes every team role check. `kbl-tool team create`,
`member add`, `agent add` and the rest call these routes; `team create` and
`member add --subject` also write the Felix side
([Adding a team with no restart](design.md#adding-a-team-with-no-restart)).

**Ingest.** A config names its team, and only sources the team owns are
read. One process may hold several blocks:

```json
{
  "teams": [
    {
      "team": "payments",
      "tracker": { "url": "http://127.0.0.1:8790", "token_env": "KBL_INGEST_TOKEN_PAYMENTS" },
      "github": { "token_file": "/run/secrets/github-token", "own_logins": ["kbl[bot]"] },
      "rules": [ { "name": "agent-ok", "repo": "acme/payments", "mode": "admit", "labels_all": ["agent-ok"] } ]
    }
  ]
}
```

Today's single-team config, with `tracker`, `github` and `rules` at the top,
stays valid and means the first team.

**Prerequisites in another team.** `work_post` with `work: "prerequisite"`
takes an optional `target_team`. Without it the target is the agent's own
team, as today. With it, the receiving team must have a grant for the agent's
team, and the rest works as in
[Prerequisites in another repo](#prerequisites-in-another-repo).

### Limits per attempt

The fold caps what one attempt may write: 200 decisions, 500 steps, 500
edits, 100 progress records, 100 context entries, 20 questions, 10
answers, 100 links, 4 different triage answers (a changed dependency list or split counts as a different answer), and 24 children and proposals together. A call over a cap is refused, and the first refusal in an
attempt is logged as refused, so a looping agent cannot fill the stream with
records or with refusals. Each field is also limited in size: text over 4 KiB
must be an artifact, and a title, command or reason is at most 4 KiB. A
question offers at most 8 options of at most 200 bytes each.

### Splitting and ordering

A large item is split by posting `split` children under it. They are live at
once and draw from the root's budget. To run them in order, the agent links
each child to the one it waits on with `work_link`; children with no edge
between them run in parallel. A child with an unfinished dependency is
Blocked, and becomes Ready the moment that dependency is done, because
readiness is recomputed from the fold and never stored. A person splits by
hand with the same records, which draw nothing from the budget. The parent
cannot complete while a child is open.

### Links across repos and sources

A link joins any two items, whatever tree, repo or source each is in: a
felix-blocks item can wait on a felix-gateway gap, or a GitHub issue on an
Azure DevOps work item. People link items of different trees freely; an
agent links across trees only through a prerequisite (below). The waiting
item is Blocked until the other one is done, the same as inside one tree.

The fold records a target for every imported root: a GitHub id (`org/repo#n`)
gives its repo, a GitLab id (`group/project#n`) the same, and an ADO id gives its `org/project`, which still selects
rules and spend. A GitHub or GitLab target is shown as a repo. A prerequisite has
its target, and any other item takes its root's. Jira ids name none, so
those items have no target. The dashboard's Blocked view (in the sidebar and
the palette, or `?view=blocked`) names each blocker with its own repo when it has one and differs from the
card's, else its source, so the board shows "waits on gw
(github:GetFelix/felix-gateway)" on the felix-blocks card and "waits on
j1 (jira)" for a Jira item. A blocker that is merged and only waits for its
release reads "waits on p1 release". The fold already holds all of this, so
the view is derived and adds no record. The item panel lists the same
blockers with their titles.

A dependency that is removed upstream is unlinked at the next poll that reads
the issue's links, if ingest's link made the edge. An agent cannot unlink:
only a person removes an edge an agent made. See Links under [Running the ingest worker](#running-the-ingest-worker) for when it stays.

### Files that overlap

Two open items that change the same files will conflict when the second one
merges. To see that early, an item can declare the files it expects to touch
in a `files` context entry: one path per line, relative to the repo root. A
path ending in `/` is a directory and covers every path under it; without the
`/`, `docs` names only a file called `docs`. Blank lines and lines starting
with `#` are skipped. An agent writes it with `work_context`, usually at
triage, and a person writes it with a `context` record.

```text
core/src/work_fold.rs
docs/
```

Paths are tidied before they are compared: `\` reads as `/`, a leading `/`
or `./` and repeated `/` are dropped, and `..` is resolved. A line whose `..`
climbs above the repo root is skipped. A last segment of `.` or `..` makes the
path a directory, as a trailing `/` does. Only `./`, `/` or `.` on its own
means the whole repo and overlaps every path declared for that repo; a line
that reaches the root another way (`a/..`) or is empty once tidied (`//`) is
skipped. Paths compare byte
for byte: there is no case folding and no glob matching, so `Docs/a.md` and
`docs/a.md` are different files and `src/*.rs` is a file with a `*` in its
name. An entry over 4 KiB is stored as an artifact, not inline, and is not
read for overlaps.

When the entries of two open items share a path (or one names a directory above
the other's file), the dashboard flags both rows ("1 file overlap") and the item
panel's "Same files" section names the other item and the shared files, and
suggests which should wait on the other. Held work goes first, then work in
review, then the rest; a tie goes to the item whose most recent `files`
declaration is older. The suggestion is advice: a person links the items with a
`linked` record, and the warning goes away. Agents cannot link across trees, and
two overlapping items are usually separate roots.

Only items whose roots land in the same repo are compared, so `README.md` in
two repos is no overlap; items with no repo are compared with each other.
Pairs already joined by links, directly or through other items, are not warned
about, since the order is recorded and the suggested link would be a cycle.
Neither are an item and its own descendants, since a split shares its parent's
files. Done and closed items are left out. The warning is computed from the
fold on read (`kbl_core::overlap`), so it adds no record and needs no format
raise.

The comparison is pairwise: every two declaring items in a repo, and every two
of their paths. The page computes it once per applied record, not per redraw,
and the panel shows at most 5 notes and 10 paths per note, with a "+N more" for
the rest. The row flag counts them all.

### Prerequisites in another repo

An agent sometimes finds that its item needs a change in another repo first: a
`felix-blocks` task needs a `felix-gateway` feature. `work_post` with
`work: "prerequisite"` handles it in one call:

| Field | Meaning |
|---|---|
| `target_source`, `target_repo`, `title`, `why`, `evidence` | A new proposal in that repo's Triage, with the reason and the steps that show it, records of the same tree as for a decision. `why` is shown with the proposal |
| `existing` | Instead of the five above: an item that already exists, to avoid a duplicate |
| `done_when` | `merged` (default) or `released`. `released` also waits for the release that carries the work. If the item already waits on the same prerequisite, the stricter ask stands: from format 5 the tracker answers a `merged` ask on a link that waits for a release without writing a record, so a retry with `merged` after `released` is not invalid |

One record does all of it:

- The proposal is created as an agent's proposal, so a person accepts it into
  the target repo like any other.
- The held item is linked to it and waits on it.
- The claim ends as `blocked`. It is not a release or a give-up, so it
  spends none of the item's two tries. The next claim resumes the same attempt
  with its decisions and `so_far`.
- It counts toward the attempt's 24 posts.

When the prerequisite is closed as merged, a link that waits for a merge frees
the item, and it is Ready again. `work_context` then lists what the item
waited on, with each prerequisite's stage, pull request and release. A link
that waits for a release stays blocked until a `shipped` record,
`{"kind": "shipped", "item": ..., "release": "0.4.0"}`, names the release that
carries the finished prerequisite. A person can write it. Ingest writes it for
a prerequisite it imported from GitHub whose pull request it saw merged: it
looks for the oldest stable release after the merge that contains it, within
the page bound in [Running the ingest worker](#running-the-ingest-worker). It
asks only for roots an open item's link is waiting on, so an ordinary merge
costs no release requests. The fold takes the record from people and ingest
only, and only for an item that is done. The tracker takes ingest's only when
the item's reported request has a merged status, and never over a release
already recorded. A person's `shipped` replaces any release recorded before,
ingest's included. An `existing` item that is already merged or completed but
has no release yet can still be named with `done_when: released`; any other
closed item is refused as `closed`. Completed covers work that is not a fix
too, so a `released` wait on it holds until a `shipped` is recorded or a person
removes the link. A person's `unlinked` drops a release wait. Re-opening the
prerequisite keeps it and clears the release, so the waiter waits for the next
one; dropping it would free the waiter on unreleased work. Accepting a finished
prerequisite applies from [format 5](upgrades.md#formats). If a person rejects
or dismisses the prerequisite, the held item goes to Needs you as "dependency
dropped".

### Prerequisites become upstream issues

A new prerequisite lives only in the tracker until someone files it in the
target repo. From [format 6](upgrades.md#formats) ingest can do that. It opens
an issue for a prerequisite when all of these hold:

- it was made by a `prerequisite` post (a deferred review finding is not one);
- a person accepted it after the log was raised to 6 (an acceptance by the
  tracker, or from before the raise, does not count; accept it again);
- an intake rule whose `repo` is the target repo has `open_prerequisites: true`;
- it is open and has no upstream issue yet.

`GET /v1/ingest/roots` lists such prerequisites under `prerequisites`. At the
end of a poll ingest opens the issue in the rule's `repo`, with the
prerequisite's title, its `why` and a last line `kbl-prerequisite: <item>`. Any
line in `why` that starts with `kbl-prerequisite:` is dropped, since an agent
wrote it. With `link_waiter: true` on the rule the body also says "Needed by"
the waiting root's issue. It is off by default so an issue in a private repo is
never named in a public one. It then writes `imported` with the existing item and
the new issue's id, which makes the prerequisite an imported root: closing or
editing the issue reaches it like any other, and a pull request for it is
followed. No intake rule takes it out of scope, since a person already
accepted the work.

Before it sends the request, ingest saves its intent to the versioned ingest
state. If another ingest process saved since this one loaded, the save fails
and this process opens nothing that poll. If ingest stops between opening the
issue and that write, it remembers the issue in its saved state and writes the
tie on the next poll. If only the intent was saved, the next listing of the
target repo shows an issue by one of `own_logins` whose last marker line names
the item, and ingest ties that one instead of opening another; an intent that
no listing explains is retried at the next reconcile. A marker issue for an
item already tied to another issue is logged as an orphan and left for a person
to close. Markers count only in issues by `own_logins` in a repo whose rule has
`open_prerequisites`. The fold checks the rest: it refuses the tie unless the
item is a prerequisite a person accepted, the issue is in the repo it targets
(owner and name compared without case), and the item has no issue yet.

This is the only cross-tree link an agent can make, and the tracker checks it
before the fold sees the record. A new prerequisite must target a source and
repository the agent's [grants](#running-the-tracker) name, or those its own
tree works in. An `existing` item must be in its own tree or in a source and
repository its grants name, and an item it cannot see is refused as
`unknown_item`, the same answer as one that does not exist. The plain
`work_link` call still refuses another tree (`cross_root`). Cycles are refused across trees too.

#### Found at triage

Finding the other repo's work before anyone builds is cheaper than finding it
halfway through a fix. So the agent that holds an item's triage work looks for
it, and posts it with the same call, naming the triage item it holds:

```json
{
  "item": "gh-812-t",
  "key": "need-cas",
  "work": "prerequisite",
  "target_source": "github",
  "target_repo": "GetFelix/felix-gateway",
  "title": "gateway needs a CAS call",
  "why": "the client cannot swap two blocks atomically without it"
}
```

The fields, the grants and the caps are the ones above. Two things differ:

- **The item under triage waits**, not the triage item. It is Blocked until
  the prerequisite is done, so no build claim is handed out for it. If an
  agent already holds the item, that claim goes on and the next one waits.
- **The triage claim goes on.** The agent still answers with `work_triage`
  and completes the triage item. Nothing is charged by this call; triage
  reports its cost when it completes.

The reply says so:

```json
{
  "item": "gh-812",
  "status": "waiting",
  "claim": "held",
  "triage": "gh-812-t",
  "prerequisite": "w3f9a1c04e7b25d8816a0",
  "created": true
}
```

The same call again answers `created: false` and writes nothing. A person who
disagrees removes the link (`unlinked`) and rejects the proposal.

This needs log format 3. Until a person
[raises the format](upgrades.md#upgrade), the call is refused with `format`
and nothing waits.

### Questions with choices

A question can offer options. `work_ask` takes `options` (at most 8, each at
most 200 bytes, none repeated) and a `default`, which must be one of them. A
person picks one with a key, or types something else. Free text is always
offered, and it is stored exactly as typed. Nothing reads it: the tracker runs
no model, so it never decides what a person meant.

An answer, from the dashboard or from `work_answer`, is one `answer` record
with either `chosen` (one of the options) or text, never both. An answer with
neither takes the question's `default`. The fold then does two things:

- It appends one claimable `resume` item under the asking item. The item
  carries three context entries: `question`, `answer` and `so_far`, which is
  the asker's last decision on that attempt. Any eligible agent may claim it,
  not only the one that asked, and `work_context` reads the three entries.
- It ends the asker's hold on the item at no cost to its tries. The asking
  item stays out of Ready while its resume item is open, so no one claims it
  beside the agent doing the resume work. When that is done, the item is Ready
  again.

Asking ends the attempt's hold once answered, so `work_ask` takes the
attempt's `usage` since its claim or its last ask, and the fold charges
it then, like a release does. A later release or give-up reports only the usage
since the ask, so nothing is charged twice. The cost floor applies to
each ask as to each claim end, so a claim that asks and then ends pays the
floor twice. A resume item sits one level below the asking item and obeys the root's
depth cap (`too_deep`). If the asking item was never accepted, as with an
unclear triage, its resume item exists but is not claimable until a person
accepts the item.

A second answer to the same question appends nothing: the question is no
longer open, and the call is refused.

If the answer says the work belongs in another repo, the person sends
`moves_to` with the source, repo and a title. The fold then creates a
[prerequisite](#prerequisites-in-another-repo) there instead of a resume item:
a proposal in that repo's Triage, with the asking item waiting on it. The
answer's text is the reason shown with the proposal. Routing a question to the
people who own a repo waits for roles (#120); until then every question goes
to My queue.

### Questions and waits free the agent

From format 14 an agent that needs a person, or something outside KBL, lets
go of the item instead of holding it while it waits:

- `work_ask` ends the claim. The item waits in Needs you and the agent can call
  `work_next` for other work. The claim it ended is given back, so asking never
  uses one of the attempt's two tries. A plain answer sends the item itself
  back to Ready with `question` and `answer` in its context, for whoever claims
  it next. An answer with `moves_to` still creates the prerequisite.
- `work_release` with `why: waits_on_person` or `why: waits_on_outside` (no
  credit, an outage) also gives the claim back. The item is not handed straight
  to another agent: it moves into Needs you as flagged, with the release's
  `reason`, until a person moves it out. `why: failed`, the default, uses a try
  as before, and a give-up always does.
- An agent whose claim lapsed while it waited (a long build, a host that went
  quiet) may still ask, or release as a wait, once, as long as nobody claimed
  the item since. The reply says `late: true`. Anything else after a lapse is
  refused as before.

Only failures count toward the two tries. An agent that calls a failure a wait
gains nothing: every wait stops in Needs you until a person acts, and questions
stay capped at 20 per attempt.

Agents must ask through `work_ask`, never through their host's own chat or
question prompt. Nobody watching the tracker sees a chat question, and a session
`dev/agents.mjs` started runs without a terminal, so it would wait forever.
Every `work_next` reply carries a `questions` line that says so, and so do the
`kbl-mcp` tool descriptions.

Triage can mark an item `unclear` with a reason, `scope`, `repo` or
`acceptance`. The item then reaches a person as a question in Needs you
("unclear"), not as Ready work, even after someone accepts it. The person's
answer starts resume work like any other.

Questions that fold before this existed carry no options and their answers
start no work, so older logs fold as they did.

What each new route and record lets a caller do:

| Route or record | Who can write it | What an agent could steer |
|---|---|---|
| `question` with `options` and `default` | The agent holding the item | The wording a person sees. Counts and sizes are bounded, and the existing 20 questions per attempt still apply |
| `answer` with `chosen` | A person, or an agent holding a `question` item under the asking item | Which option is recorded. It must be one the question offered |
| `answer` that starts resume work | The same | The text the resume agent reads, as any answer can. There is one resume item per question |
| `answer` with `moves_to` | A person only. The agent route refuses it (403) and the fold refuses it from an agent (`actor_not_allowed`) | Nothing directly. An agent can shape the question a person reads, but only the person puts work in another repo's Triage |
| `triaged` with `unclear` | The agent holding the triage item, or a person | That an item waits for a person. It cannot make an item Ready or move it to another repo |
| `prerequisite` from triage | The agent holding the triage item | That the item under triage waits on work in a repo the agent's grants name, and that a proposal appears in that repo's Triage. A person accepts or rejects the proposal, and can remove the link |
| the `resume` item | The fold only. `work_post` does not accept it | Nothing: no caller creates it |

### Standing rules

A repo has rules every agent on it must follow: where to work, where builds go,
which git commands are off limits, what to run before a push. Without a place
to keep them, each brief repeats them and drifts. The tracker stores them once
and sends them with every claim, so a brief carries only the issue.

Rules have two parts, each optional:

- `text`: what the agent must follow, inline and at most 8 KiB. Rules are
  never an artifact: a claim must carry them as written, and a short summary
  is enough.
- `checks`: commands to run before pushing, at most 20, each at most 500 bytes.
  The agent runs them. The tracker never does.

A person sets them with `POST /v1/work/rules {source, repo, root?, text?, checks?}`,
which writes a `rules_set` record. Without `root` the rules are the repo's; with
it they are that root's own. The latest record for a scope wins and its offset
is the version. A record can set one part and leave the other alone. For an
item, the root's parts override the repo's part by part: a root that sets only
`checks` keeps the repo's `text`, and a root's `checks: []` clears the repo's
checks for that root. A record with empty `text` and no checks removes its
scope, so a root falls back to the repo's rules and the scope's slot is free. At most 500 scopes have rules.

Every claim reply, and so the `work_next` tool, carries what is in effect. The
`work_rules` tool reads the same for an item the agent holds:

```json
"rules": {
  "text": "One worktree per lane, off origin/main. ...",
  "checks": ["task lint", "cargo test -p felix-storage"],
  "version": 412,
  "repo_version": 380,
  "root_version": 412
}
```

`rules` is `null` when nothing is set. `version` is the newest record that
contributes, so an agent that resumed a claim can tell its rules changed.
Agents read rules and cannot write them: the fold takes `rules_set` from a
person only, and the tracker refuses it from an agent or the ingest token.

For example, the Felix workspace's lane rules, summarised:

```json
{
  "source": "github",
  "repo": "GetFelix/felix",
  "text": "Work in your own git worktree off origin/main, named <lane>-<issue>. Export CARGO_TARGET_DIR (one per worktree) before any cargo or git command. Never git stash, never force-push, never run git in the main checkout. Commit with the personal email and no AI attribution lines. Pass -R GetFelix/<repo> on gh writes. A regression test must fail with the fix reverted. Docs ship in the same PR.",
  "checks": ["task lint", "task docs:evidence", "cargo test -p <crate>"]
}
```

The full file is longer: machine limits, scratch directories, review and merge
steps. Keep the rules to what fits in 8 KiB. A root can add its own `checks`,
for example the one crate a fix touches, without repeating the rest.

### Review as work

A review is claimable work with a reviewer tier. KBL never runs a model:
the tier is a name such as `fast` or `deep` that says which kind of reviewer
should claim it, and the agents decide what each name means.

```mermaid
flowchart LR
    PR[Request opens<br/>pr_status: open] --> F[First-pass review<br/>tier fast]
    F -->|finding: fix| X[Fix, child of the root<br/>for the author]
    F -->|finding: defer| D[Proposal<br/>a person accepts]
    F --> L[Final review<br/>tier deep]
    X --> L
    L -->|finding: fix| X2[Fix, child of the root]
    X2 --> L2[Fresh final review<br/>waits for the fix]
    L --> V[Verdict: merge or block]
    L2 --> V
    V --> P[A person merges]
```

- **Created with the request.** When ingest reports an item's pull request
  open, the tracker adds two `review` children under the root: a first pass
  for the first configured tier and a final review for the last. The final
  waits for the first, and for every fix the first pass asks for, so it reads
  the tree with the fixes in. Each request gets them once, and a root has at
  most 8 review items, fresh final reviews included. A person or an agent
  cannot create one.
- **Claimed by tier.** The tiers are `KBL_REVIEW_TIERS` (default
  `fast,deep`, at most four), one ready stream each (`work.review.r0` to
  `r3`). `work_next` gives a review only to an agent that names its tier in
  `review_tiers`, so ordinary work never blocks behind a review and a review
  never reaches an agent that did not ask. An unknown tier is refused.
- **Never your own work.** An agent that claimed the root, or any fix or
  resume under it, is never given a review of that tree. The tracker skips
  it, and the fold refuses the claim as a second check. An agent that takes a
  fix a review asked for becomes an author too, so it cannot give the final
  review.
- **Findings are records.** `work_finding` writes severity (low, medium,
  high, critical), file, line, title, the scenario that goes wrong, and a
  verdict. A review holds at most 40 findings, a file is at most 400 bytes, a
  title 200, a scenario 4096. A finding is about the request the root still
  reports; a review of an older one is refused.
  - `fix` becomes a child of the root, accepted and ready, that draws one
    item from the root's budget. Its claim reply gives the request's branch,
    so the author goes on from there. Any agent may take it, the author
    included. It is done when its attempt is, since it lands in the request.
    A fix filed by the final review makes one fresh final review, however
    many fixes it asks for. It waits for them, so the verdict that can lead
    to a merge always reads the tree with the fixes in. The review that asked
    for them ends with a `block`.
  - `defer` becomes a proposal in the root's repo that waits in the
    Proposed column for a person. It draws nothing from the budget.
  - `dismiss` is recorded and does nothing else.
- **Revert checks.** A fix's `decide` call can carry `revert_check`: the test,
  whether it failed with the fix reverted, and whether it passed with it
  restored. The attempt lists the decisions where both held, so the
  dashboard shows which fixes a test guards.
- **The verdict.** A final review ends with `work_verdict`, `merge` or `block`
  and a reason, and cannot complete without one. A merge is refused while a
  fix a review asked for is open. The verdict sits on the root with the
  request it was about, and goes stale if the root reports another. The
  dashboard shows it; merging stays a person's action, and nothing in this
  flow closes the root.

Threat model: a reviewer is an agent, so everything it writes is bounded and
attributed to its token. It can fail to find a bug or recommend a merge it
should not, which is why the verdict is advice. It cannot merge, close, accept
its own proposal, or review its own work. Two tokens run by one person count
as two agents, so keeping an author and its reviewers on separate tokens is
the operator's job.

### Kinds of work

| Kind | Claimed by | The answer comes back as |
|---|---|---|
| `fix` | A coding agent | Progress records, a branch, a PR |
| `triage` | Any agent | A proposal written with `work_triage`: a tier with one line of why, a possible duplicate, `depends_on` (items it must wait on), `related` (items to schedule with, which block nothing), `split` (titles it should be cut into), or `unclear`. A person accepts, edits or rejects it, and decides about the dependencies and the split |
| `augment` | Any agent | Context records on the item: repro steps, related code, logs, linked issues, affected areas |
| `split` | The agent holding the item | Live child items, within the budget below |
| `question` | Any agent | An answer record that cites stream and offset |
| `resume` | Any agent | Whatever the asker was doing, carried on from the answer. Only an answer creates it |
| `test` | Any agent | Test follow-up for an item, as progress records |
| `review` | An agent that names the reviewer tier, and did not work on the request | Findings, and for a final review a verdict. Only the tracker creates it. See [Review as work](#review-as-work) |

Root-shaping changes (a tier, or a split of a root at triage) and out-of-scope
work are proposals that a person accepts. Children under a held item are live.

An agent answers a triage item with `work_triage`, naming the triage item it
holds (or nothing, for the item it holds). The tracker writes `triaged` on the
item under triage and refuses the call from anyone else, for an item that is
not triage work, and for a `duplicate_of` that is not another item the agent can see. The same
answer sent twice by the same agent is written once. The agent then completes the triage item.

```json
{
  "tier": 1,
  "why": "data loss on restart, no workaround",
  "duplicate_of": "gh-792",
  "depends_on": ["gh-801"],
  "related": ["gh-790"],
  "split": ["fix the reader", "fix the writer"]
}
```

`depends_on`, `related` and `split` are advice. They are recorded with the proposal, and the
tracker links nothing and creates no item from them. A person who agrees links
the dependencies (`linked`) and posts the pieces. Each list holds at most 8
entries, a dependency must be another item the agent can see (the same rule as
`duplicate_of`), and a split title is at most 256 bytes. Changing either list
counts as a different answer toward the 4 an attempt may log.

**Which to use.** Name `depends_on` only when the item cannot start until the other is done: once a person links it, the item stays Blocked until then. Name `related` when the two merely belong together, for example because both should run on the same machines or be worked by the same agent: nothing waits. A `related` entry cannot also be in `depends_on`. Naming `related` needs the log raised to [format 18](upgrades.md#formats); below it the call is refused with `needs_format_18` and writes nothing.

#### Adjusting triage's advice

From format 17 a person edits the two lists before accepting, with
`POST /v1/work/proposals/edit` and `depends_on` and `split`. The item is the
root triage answered for, and the request must carry `proposal`, the offset of
the answer the person read. The list a request leaves out stays; an empty list
clears it. The edit is advice just like the answer: nothing is linked or created.

The same bounds hold as for the agent: at most 8 entries a list, distinct
dependencies, a split title of 1 to 256 bytes. The fold also refuses a
dependency that is the item, its own triage item, or no item in the log
(`bad_dependency`), and an edit with no `proposal`, or one that is not the
standing answer's offset, as `stale_proposal`. That last check is what keeps an
edit made against an answer triage has since replaced from landing on the new
one. The record changes the standing answer in place, so its offset stays and a
second edit may name it again. If triage then answers anew, its answer replaces
the edited one whole, as any new answer does. Below format 17 the record is
refused with `format`.

**What triage does to a root's tier.** Ingest admits a root at the tier its
rule gives, so a root in `admit` mode is accepted before anyone triages it.
The tier in the triage answer is advice there too: it never changes the
admitted tier, and accepting again without a tier keeps it. A person who takes
the advice sets it with `tier_changed` (`POST /v1/work/tier`). A
root held in Triage by `propose` mode has no tier yet, and accepting it uses
the proposal's tier, as before.

Triage and augmentation are claimed, extended, completed and given up like
any other work. A dead agent's triage goes back to the queue. Nothing waits on
the tracker to think, because the tracker does not.

### Choosing what to take

`work_next` takes the highest-priority ready item, and the roots in a tier
take turns. After an item of one root is handed out, the next claim in that
tier prefers another root's ready item, the root served longest ago first, so
an agent that keeps calling `work_next` is not given the children of the same
two roots every time. Within a root the item that has been ready longest goes
first. The turns are kept in the tracker's memory, so a restart starts them
over. Work that a person's answer made ready (a resume item, or an item with
the question and its answer in its context) queues behind other roots' ready
work in its tier, and goes first in turn once it has waited one visibility
period, so an answer is never left unread.

A caller can narrow the call, and none of these let it take what it could not
take without them:

| Field | Takes only |
|---|---|
| `kinds` | Work of these kinds: `triage`, `augment`, `fix`, `split`, `question`, `test`, `resume` or `review` (a review also needs `review_tiers`) |
| `tiers` | Work in these priority tiers, 0 the highest |
| `repo` | Work that lands in this repo, as `owner/name`, in any letter case |
| `exclude_roots` | Work outside these roots, at most 64 |
| `milestone` | Work whose root has this upstream milestone, in any letter case |
| `labels` | Work whose root has at least one of these upstream labels, in any letter case, at most 32 |
| `item_type`, `area`, `size` | Work whose root was classed this type, area or size, in any letter case |
| `capabilities` | Work whose needs are all in this list, at most 32 plain words. Not a narrowing: see [What the work needs](#what-the-work-needs-and-what-an-agent-can-do) |

An item assigned to the agent is filtered the same way, so a caller that asks
for triage is not handed its assigned fix. An unknown kind or tier is refused
with 400 before anything is tried.

`work_peek` (`POST /v1/work/peek`, agent token) takes the same narrowing fields
and `limit` (default 10, at most 50). It lists the next items the caller would
be handed, in order: `item`, `title`, `work`, `tier`, `root`, `repo`, and
`assigned_to_you` for the items it would take first. It claims nothing, holds
nothing, and touches no ready record. Its order is the one a claim follows
unless work arrives or changes before the claim.

**How this stays safe with the ready lanes.** Felix hands a lane's records
out in offset order and counts every handout as an attempt, so the tracker
cannot look at a record and put it back for a caller that did not want it.
When a call has a filter, or a lane has ready items from more than one root,
the tracker chooses the item from its state and claims it directly, as it
does for an assigned item (see [Assignees](#assignees)): no ready record is
polled, acked or nacked, so an item the caller filtered out or that waited its
turn spends none of Felix's handouts. The claimed item's own record stays in
its lane and is acked as stale when it is next polled. The `claimed` record is
written conditionally on the tail and the fold refuses a claim of a held item,
so two agents still never hold one item. A lane with one root ready and no
filter is polled as before. A direct claim has no Felix delivery, so the sweep
lapses it when it is not extended, as described under assignees.

Filters and fairness across roots need the log raised to format 15. Below it,
a call with a filter is refused with `needs_format_15`, and a lane with several
roots is polled in its own order. Polling and passing over non-matching items
would spend ready records, and a direct claim on an unassigned item would not
survive a restart, so neither is done. The peek is read-only and works at any
format.

## The Felix mapping

It reuses the pieces the run resources already use. No database.

- **`work.events` is the record.** Every change is a record: imported,
  triaged, linked, context added, claimed, progress, done. The board view and
  the per-item history are folds of this stream, built the way the run log
  replay is.
- **Items and edges are keys.** An item's fields and its context entries are
  keys in a cache, written with `cache_put_if` at the version read, so a
  person and an agent editing the same item cannot overwrite each other. This
  is the board pattern.
- **Only ready items are claimable.** The tracker folds `work.events`, and when
  an item's last dependency is done it publishes the item to a ready stream.
  The tracker claims from that stream for agents through one consumer group,
  as a member named after each agent, with the 90 second visibility and 30
  second extension that tasks use. A dead agent's item goes
  back to the queue.
- **Priority is a stream per tier.** A group hands records out in offset order,
  so `work_next` polls `work.ready.p0` first, then `p1`, and so on, then the
  review streams the agent named.
  Reprioritizing a ready item publishes it again on its new tier. The old
  record is acked and skipped when claimed, because its item version no
  longer matches.
- **Upstream ids dedupe.** Ingest writes each upstream id with `cache_put_if`
  absent, so a re-polled or redelivered bug is imported once.
- **Writing back to GitHub or ADO carries the claim's version.** That version
  is the fencing token, as with leases: the tracker refuses a comment or
  status change from an agent whose claim has lapsed.

A fixing agent's work is a run, so the existing replay shows why it did what
it did. The tracker links each item to its runs.

## Sources

GitHub, GitLab, Azure DevOps and Jira are each an adapter behind one interface, so the
tracker, the tools and the dashboard never see which system an item came
from. An adapter lists what changed since a cursor it keeps, fetches one item,
maps its fields and links, and writes back. Polling is the record; webhooks
(GitHub and GitLab webhooks, ADO service hooks, Jira webhooks) only wake the poll early,
so a missed webhook costs latency, not an item.

| | GitHub | GitLab | Azure DevOps | Jira |
|---|---|---|---|---|
| Upstream id | `github:GetFelix/felix#812` | `gitlab:group/project#812` (the issue's `iid`) | `ado:org/project#4410` | `jira:acme/FEL#123` (site `acme`, key `FEL-123`) |
| Scope by | repo, labels (all or any), excluded labels, milestone, assignee, type | project, labels (all or any), excluded labels, milestone, assignee, type | project, area path, iteration, tags, or a saved query | a JQL filter |
| Priority from | a label mapping | a label mapping (`priority::1`) | the Priority field (1 to 4) | the priority field |
| Links from | blocked by, sub-issues | is blocked by | predecessor, parent | blocks, sub-tasks |
| Closed as | `completed` / `not_planned`, duplicate | completed, or dropped by a `wontfix`, `duplicate` or `not planned` label, or a move | Done, Closed, Removed | the resolution |
| Pull requests | pull requests | merge requests | none (the GitHub worker records them) | none (the GitHub worker records them) |
| Credentials | GitHub App or token | group or project access token | personal access token or Entra service principal | API token or OAuth app |

The ingest worker holds the source credentials. Agents never see them and
write back only through the tracker, which checks the claim first.

### Jira

A worker serves one source, so a Jira worker has a `jira` block where a
GitHub one has `github`. A rule's `repo` is `<site>/<PROJECT>`, and its
`jql` is the filter, combined with the project:

```json
{
  "tracker": { "url": "http://127.0.0.1:8790", "token_env": "KBL_INGEST_TOKEN" },
  "jira": {
    "site": "acme",
    "api": "https://acme.atlassian.net",
    "email": "kbl@acme.example",
    "token_env": "JIRA_API_TOKEN",
    "own_logins": ["KBL"],
    "pr_url_base": "https://github.com"
  },
  "rules": [{
    "name": "agent-ok",
    "repo": "acme/FEL",
    "jql": "labels = agent-ok AND issuetype in (Bug, Task)",
    "mode": "admit",
    "cap": 10,
    "default_tier": 2,
    "priority_labels": { "Blocker": 0 },
    "components": { "storage": "GetFelix/felix", "docs": "GetFelix/felix-docs" },
    "target_repo": "GetFelix/felix",
    "comment": true,
    "link_pr": true,
    "transition_on_pr": "In Review",
    "transition_on_merge": "Done"
  }]
}
```

- **Sign-in.** Either an API token (`email` plus `token_env` or
  `token_file`, sent as HTTP basic) or an OAuth app's access token
  (`oauth_token_file`, sent as a bearer, with `api` set to
  `https://api.atlassian.com/ex/jira/<cloud id>`). The file is read again
  for every request, so something beside the worker can refresh it. The
  secret is never in the config. The worker needs to know its own name to
  tell its comments from others: it reads it from Jira (an OAuth token needs
  the `read:me` scope) or from `own_logins`, and refuses to start with
  neither. Set `browse_url` to the site's address when `api` is the OAuth
  one, since issue links are built from it.
- **Selection.** Jira runs the JQL. Ingest asks for the project's changes
  since its cursor and, per rule, which of them the filter picks, so an
  issue that stops matching is withdrawn like a GitHub issue that loses its
  label. The first poll reads open issues only. Results are paged and a
  cut-short listing resumes from the newest update it read. A `jql` may not
  contain `ORDER BY`, since ingest orders the search itself. A search that
  fails (a 401, 403 or 400) stalls that project with its issues and cursor
  as they were; it never takes issues out of scope.
- **Tiers.** The priority name maps through `priority_labels` first, then
  the defaults (Highest, Blocker and Critical 0; High and Major 1; Medium 2;
  Low, Lowest, Minor and Trivial 3); anything else takes `default_tier`. Labels map too, and the lowest tier wins.
- **Links.** A `Blocks` link whose inward issue is the blocker, and
  sub-tasks, become edges. Closed means the status category is Done:
  resolutions Done, Fixed, Complete, Completed and Resolved count as
  completed, any other (Won't Do, Duplicate) as dropped.
- **Repository.** The first of the issue's components found in `components`
  names the repository, then `target_repo`. An issue with neither is
  imported and shown in Needs you as out of scope; it returns to its stage
  on the first poll after a mapping covers it.
- **Write-back.** For an agent's pull request in a mapped repository,
  ingest adds a remote link (`link_pr`), a comment (`comment`) and moves the
  issue through the named workflow transition (`transition_on_pr`, and
  `transition_on_merge` once the item's pull request is recorded as merged).
  A transition the workflow does not offer from the issue's status is
  skipped. Each step is done once per pull request. A Jira worker reads no
  pull requests and holds no GitHub credential. The GitHub worker records the
  state of an agent's request on a Jira root, so `transition_on_merge` needs
  a GitHub worker running beside it with a rule for the repository the
  request is in. That worker writes the request's `pr_status` on the root (the
  tracker then opens the request's review items, as for any root) and nothing
  else; the Jira worker alone moves the issue and the root.
- **Limits.** A 429, or a 503 with `Retry-After`, makes the worker wait that
  long (30 s when none is named, at most an hour) before any further request.
  Re-polling and a lost cursor never import an issue twice.

### Azure DevOps

An Azure DevOps worker has an `ado` block where a GitHub one has `github`. A
rule's `repo` is `<organization>/<project>`; the work item id is the number,
so `ado:contoso/Platform#4410` is work item 4410 in project `Platform`. A
project whose name has spaces is listed under `projects`, which maps the name
the rule uses to the real one:

```json
{
  "tracker": { "url": "http://127.0.0.1:8790", "token_env": "KBL_INGEST_TOKEN" },
  "ado": {
    "organization": "contoso",
    "projects": { "Platform": "Platform Team" },
    "pat_env": "ADO_PAT",
    "own_logins": ["KBL Bot"]
  },
  "rules": [{
    "name": "agent-ok",
    "repo": "contoso/Platform",
    "labels_all": ["agent-ok"],
    "mode": "admit",
    "area_paths": {
      "Platform Team\\Storage": "GetFelix/felix",
      "Platform Team\\Docs": "GetFelix/felix-docs"
    },
    "target_repo": "GetFelix/felix",
    "comment": true,
    "link_pr": true,
    "transition_on_pr": "Resolved",
    "transition_on_merge": "Closed"
  }]
}
```

- **Sign-in.** Either a personal access token (`pat_env` or `pat_file`, sent
  as HTTP basic) or a Microsoft Entra service principal (`entra`: tenant,
  client id, and `client_secret_env` or `client_secret_file`). Ingest signs
  in with the client credentials flow for the Azure DevOps resource
  (`499b84ac-1321-427f-aa17-267ca6975798/.default`), keeps the token until
  two minutes before it expires, and signs in again once on a 401. Azure
  DevOps no longer takes new OAuth app registrations, so there is no OAuth
  option. Secrets are read from the environment or a file each time they are
  used, never from the config, and are not in any error or log line. The
  worker learns its own name from `connectionData`, or from `own_logins`, and
  refuses to start with neither.
- **Least privilege.** A worker with no rule that writes back needs only
  Work Items (Read) on the PAT, and the adapter refuses every write. Give
  Work Items (Read & write) only to a worker whose rules set `comment`,
  `link_pr` or a transition. A service principal has no scopes to pick: its
  rights are the access level and project permissions it was added with, so
  add it as a Basic user with read access, and write access only for
  write-back.
- **Selection.** Either tags or a saved query. Tags are the rule's
  `labels_all`, `labels_any` and `labels_excluded`, matched against the work
  item's tags; the type, assignee and iteration (`milestone`) filters work as
  for GitHub. `ado_query` names a saved query by id or by path
  (`Shared Queries/Agent Work`) and replaces those filters: the query
  decides, so an item that leaves it is withdrawn. Ingest asks for the
  project's changes since its cursor with a WIQL query it builds itself, and
  the saved query's own result is intersected with them. Nothing from a work
  item, and no cursor that is not a date, is ever put in a query or an
  address. The first poll reads open items only. A query that fails (401, 403
  or 404) stalls that project with its items and cursor as they were.
  A poll reads at most 2000 items; when that many share one change instant,
  the cursor also keeps the last id read there and the next poll resumes
  after it, so a wide tie advances. A state change the process refuses with a
  rule error (a transition it does not allow from here) is reported once and
  dropped; any other failed state change is retried on later polls, a few
  times.
- **Tiers.** Priority 1 to 4 maps to tiers 0 to 3. `priority_labels` can map
  a priority (`"2": 0`) or a tag, the lowest tier wins, and anything else
  takes `default_tier`.
- **Links.** A predecessor link and an item's children become edges: the
  item waits on its predecessors and on its children. An end in a project
  that no rule reads is not an edge.
- **Closed.** The state Done, Closed or Completed is completed; Removed or
  Cut is dropped; any other state is open.
- **Repository.** The longest `area_paths` entry that is the item's area
  path or an ancestor of it names the repository (`Platform\Team` covers
  `Platform\Team\Sub`, not `Platform\Teamster`), then `target_repo`. An
  item with neither is imported and shown in Needs you as out of scope, and
  returns to its stage on the first poll after a mapping covers it.
- **Write-back.** For an agent's pull request in a mapped repository, ingest
  adds a hyperlink to the work item (`link_pr`), a comment (`comment`) and
  sets the named state (`transition_on_pr`, and `transition_on_merge` once
  the item's pull request is recorded as merged). Each is done once per pull
  request, skipped when it is already there, and a state the work item's
  process refuses from where it is is skipped. Only work items ingest
  imported are written to, and only in a project a rule reads. As with Jira,
  an Azure DevOps worker reads no pull requests: the GitHub worker records
  their state, so `transition_on_merge` needs one beside it.
- **Limits.** A 429, or a 503 with `Retry-After`, makes the worker wait that
  long (30 s when none is named, at most an hour). A bad token is answered
  203 with a sign-in page or a redirect; both are read as a 401 and stall the
  project.

### GitLab

A GitLab worker has a `gitlab` block where a GitHub one has `github`. A rule's
`repo` is `group/project`, and the issue id is the project's issue number
(`iid`), so `gitlab:team/app#812` is issue 812 of that project. A project in a
subgroup, whose path has more than one slash, is listed under `projects`,
which maps the name the rule uses to the project's full path. GitLab.com is
the default; a self-managed instance sets `api`:

```json
{
  "tracker": { "url": "http://127.0.0.1:8790", "token_env": "KBL_INGEST_TOKEN" },
  "gitlab": {
    "api": "https://gitlab.example.com",
    "projects": { "team/app": "group/sub/app" },
    "token_file": "/run/secrets/gitlab-token",
    "own_logins": ["project_42_bot_8f3a"]
  },
  "rules": [{
    "name": "agent-ok",
    "repo": "team/app",
    "labels_all": ["agent-ok"],
    "mode": "admit",
    "cap": 10,
    "default_tier": 2,
    "priority_labels": { "priority::1": 0, "priority::2": 1 },
    "comment": true,
    "transition_on_merge": "close"
  }]
}
```

- **Sign-in.** A group or project access token, which GitLab creates with a
  bot user, in `token_env` or `token_file`. The file is read again for every
  request, so a rotated token is picked up, and the secret is never in the
  config, the saved state, a record, an error or a log line. The worker
  learns its own username from `GET /user`, or from `own_logins`, and refuses
  to start with neither.
- **Address.** `api` is the instance's address and may carry a path prefix
  (`https://example.com/gitlab`). It must be https; plain http is accepted
  only on loopback (`localhost`, `127.0.0.1`, `::1`), and an address with a
  user name, password, query or fragment is refused. Redirects are not
  followed, so the token is never sent anywhere the config did not name. The
  address comes from this file alone: no request is built from an issue's
  text, and a page of a listing is asked for by number.
- **Least privilege.** A worker with no rule that writes back needs only
  the `read_api` scope, with the Reporter role, and the adapter refuses
  every write. Give the token `api` only to a worker whose rules set
  `comment` or a transition, or `open_prerequisites`. Without `api`, a
  dropped issue's merge request is not made a draft either. Give the bot
  access to the projects the rules read and no others: what the bot can see
  is what ingest can import, confidential issues included.
- **Selection.** The rule's own filters, run by ingest over each project's
  issues: labels (`labels_all`, `labels_any`, `labels_excluded`), `milestone`,
  `assignee` and `type` (`issue`, `incident` or `task`), as for GitHub. The
  first poll reads open issues only, and later ones ask for what changed
  since the cursor, which includes a close. Results are paged; a cut-short
  listing resumes from the newest update it read. A project that answers 401,
  403 or 404 stalls with its issues and cursor as they were. A note is read
  only for an issue a rule takes and that has notes; a system note and an
  internal note are not imported.
- **Tiers.** `priority_labels` maps a label (a scoped label such as
  `priority::1` works) to a tier, the lowest tier wins, and anything else takes
  `default_tier`.
- **Links.** An issue link of type "is blocked by" becomes an edge: the
  issue waits on the issue that blocks it. "Blocks" is the other issue's
  wait and shows in its own listing, and "relates to" waits on nothing, so
  neither is an edge. A blocker in a project that no rule reads is not an
  edge, and its issue's links are then not whole.
- **Closed.** A closed issue is completed, unless GitLab moved it or it carries a
  `wontfix`, `duplicate`, `invalid` or `not planned` label (or the same under
  `resolution::`), which is dropped. GitLab keeps no reason on a closed
  issue, so a label is the only signal.
- **Merge requests.** Merge requests stand in for pull requests, and the
  worker reads the one an agent reports on a root, in the issue's own
  project: `team/app#7` is merge request 7 there. The rules are GitHub's.
  Only a request our own login opened is read further. It is tied to the item
  by the branch `kbl/<item>` or a `Closes #812` in its description, and must
  have been created after the first claim. A merge counts only when it went
  into the project's default branch and GitLab names a merger who is not one
  of our logins, so a merge into a side branch, a merge by our own token, a
  merge GitLab names no merger for and a request of someone else's all leave
  the root open. The merge request's reviews (approvals and requested
  changes, never the author's own) and pipeline (the first failed job that is
  not allowed to fail) are recorded as its review and checks.
- **Write-back.** For an agent's merge request in the rule's project (or its
  `target_repo`), ingest posts a comment (`comment`) and sets the issue's
  state (`transition_on_pr` and `transition_on_merge`, each `close` or
  `reopen`). The comment names the request as `group/project!7`, which GitLab
  links. Each is done once per merge request, a comment already there is not
  posted again, and a state the issue is already in is skipped. When an issue is
  dropped while our own request is open, the request becomes a draft with a
  note. Only issues ingest imported are written to, and only in a project a
  rule reads. `link_pr` is for Jira and Azure DevOps: GitLab links a request
  from the comment and from `Closes`.
- **Limits.** A 429, or a 503 with `Retry-After`, makes the worker wait that
  long (30 s when none is named, at most an hour) before any further request.
  A redirect is read as a refusal (a sign-in page or a moved instance) and
  stalls the project.

### Labels, milestone, type, area and size

An item carries two kinds of facts that help an agent choose and a person see the shape of the queue.

**From upstream, at import.** The `imported` record carries the issue's labels and milestone, so an item has them
before any triage. The sources map their own words: GitHub and GitLab labels and milestone, Jira labels and fix
version, Azure DevOps tags and iteration path. An item keeps at most 32 labels of at most 64 bytes and a milestone of
at most 100 bytes. When the issue's labels or milestone change, reconcile writes an `edited` record with the new values
(an empty milestone clears it). Only ingest may write those fields, so a person cannot edit upstream's facts. A rule
can map them as it admits an item: `milestone_tiers` (milestone to tier; the lowest tier among the milestone and the
priority labels wins), `label_types` and `label_areas` (label to type or area; the first label upstream lists that the
rule maps). A mapped type or area must be in the vocabulary below, or the tracker refuses it and the item imports
without it. A log below format 19 imports the issue without the facts, and ingest writes them after the raise.

**Classified, from a vocabulary.** The team declares what triage may choose from in the standing rules:

```json
{ "source": "github", "repo": "GetFelix/felix",
  "vocabulary": { "types": ["bug", "feature", "perf"], "areas": ["storage", "dashboard"], "sizes": ["S", "M", "L"] } }
```

`POST /v1/work/rules` takes `vocabulary` like `text` and `checks`, and a root's own vocabulary overrides its repo's.
Each list holds at most 32 plain values (letters, digits, `-`, `_`, `.`, `/`), and `sizes` defaults to `S`, `M` and
`L`. Triage sets `item_type`, `area` and `size` in `work_triage` from that list. A value outside it refuses the whole
answer as `bad_class`, and a team with no list for types or areas accepts none. A person overrides any of the three
with `classify_item` (`POST /v1/work/classify`; an empty value clears it). The fold remembers who chose each value (a
rule, triage or a person): triage replaces a rule's value, nothing replaces a person's, and the dashboard shows a value
the same way whoever chose it. Only a root takes a class: classifying a child item (a triage or augment item, an
attempt's work) with `classify_item` is refused as `not_a_root` before anything is written, because the filters below read the root's class and a value on a child
would never match. These fields are format 19.

`work_next` and `work_peek` filter on all of them, read from the item's root, so a root's triage and augment items
follow it. A filtered claim is made from the state, so the ready records of items it passes over are not polled, acked
or nacked. A peek shows each item's milestone, labels, type, area, size and needs.

### What the work needs, and what an agent can do

Some work only runs somewhere: it needs Azure credit, a GPU, a network the
others lack. A root can say so with a set of `needs`, and an agent says what it
can do now with `capabilities` on the call. `work_next` and `work_peek` hand it
only work whose needs are all in its capabilities.

- **Needs** are words from a per-team list, `vocabulary.needs` in the standing
  rules, next to the types and areas above (`{"vocabulary": {"needs":
  ["azure", "gpu", "network"]}}`). A set holds at most 8, each from the list, in
  any letter case, stored as the list spells it. A value outside the list
  refuses the whole record as `bad_class`, and a team with no list accepts none.
- **Who sets them.** Triage with `needs` on `work_triage` (an empty list drops
  what triage set before), a person with `classify_item` and `needs` (an empty
  list clears them), and an intake rule that maps labels: `label_needs` is a map
  from label to need, and an issue needs every need its labels map to
  ([ingest setup](ingest-setup.md#mapping-labels-and-milestones)). The fold
  remembers who chose the set, with the precedence of the class values above:
  triage replaces a rule's set, and nothing replaces a person's. Needs are one
  set per root, not merged between actors.
- **What they cover.** The root's needs apply to the work that runs in an
  environment: its fix, test and resume items. Triage, augment, split, question
  and review items need nothing, so any agent can triage or review work it
  could not do.
- **Capabilities** go on each `work_next` and `work_peek` call, not on the
  agent's registration, so a session can say it has a GPU now and not later.
  They are plain words compared without regard to case. With none declared, an
  agent is handed only work that needs nothing, so adding a need to an item
  never reaches an agent that did not ask for it. An item assigned to an agent
  without the capability is not handed to it either.
- **Skipped work spends nothing.** A call whose capabilities do not cover a
  ready item claims from the state instead of polling the lane, so the passed-over
  item's ready record is neither polled, acked nor nacked, and the answer is
  the same after a restart. If a need is added after a record was polled, the
  record is acked like a blocked item's and the item is published again when
  the state next syncs.
- **Format 20.** `needs` on `edited` and `triaged`, a `needs` list in a
  vocabulary and `resource_lifted` are refused with `format` until a person
  raises the log to 20 ([upgrades](upgrades.md#formats)). A reader below 20
  stops at the first such record, so it never hands an item to an agent that
  lacks what it needs.

### Intake rules

Each source has one or more intake rules. A rule is the source's own filter
(the "Scope by" row above, combined with AND) plus:

- **Mode.** `admit` makes a matching issue a root at once, with one triage
  item and one augment item; it suits a label a person applies on purpose,
  such as `agent-ok`. `propose` puts it in Triage as a proposal that becomes a
  root only when a person accepts it; it suits broad filters such as "the 0.8
  milestone". Add `"triage": "skip"` to an `admit` rule for issues that are already well specified, such as a curated milestone or an `agent-ok` label a person applied after writing the acceptance: the root skips triage and augment and an agent can fix it at once.
- **A cap** on the open roots the rule may have at a time, so a broad filter
  cannot flood the queue. Matches over the cap wait upstream and are picked up
  as roots close.
- **A default tier** and the mapping from the upstream priority, plus maps from milestone to tier and from labels to type and area ([labels and class](#labels-milestone-type-area-and-size)).
- **The repository** agents work in. ADO and Jira items do not name one, so
  the rule maps an area path or component to it. An item with no
  repository goes to Needs you.

Each upstream id is imported once (`cache_put_if` absent on the id), so
re-polls and redelivered webhooks never duplicate an item. An issue that
matches two rules belongs to the first one listed.

### When the upstream issue changes

| Upstream | KBL |
|---|---|
| Title, body or labels edited | An `updated` record. The item shows "changed upstream since triage". Triaging again is a person's action, so edits never create work |
| A new comment | A `context` record credited to the upstream author. Agents see it on their next read |
| No longer matches its rule (label removed, milestone moved) | `left_scope`. Withdrawn from Ready if nobody holds it. If an agent holds it, the attempt finishes or is released and the item then stops. Shown in Needs you |
| Matches its rule again | `returned_to_scope`: out of Needs you and back to its stage. Needs a log at format 2 |
| Closed as completed or fixed, or closed with no reason (issues closed before GitHub kept one) | `closed` with reason completed: the item is done and its dependents unblock |
| Closed as not planned, duplicate or won't fix | `closed` with reason dropped: open children and attempts are cancelled, dependents go to Needs you ("dependency dropped"), and an agent's open pull request becomes a draft with a comment |
| Reopened | Back to Triage as the same root, with its history and its remaining budget |
| Moved or transferred | The same item (the tracker still shows the original id), with a `moved` context entry naming the new one. Later closes and reopens under the new id apply to the item, across ingest restarts too, when a rule names the new repo. Otherwise a close arrives when its turn in the `reconcile_hours` pass comes and a reopen is not seen |

Ingest skips upstream events written by KBL's own credential, so its
comments, pull request links and state changes do not come back as edits.

**Issue text is untrusted.** Anyone who can file or edit an issue can write
instructions for the agent that reads it. KBL runs no model, so it cannot
be steered, but agents can. Intake rules decide whose issues get in; budgets,
grants and review bound what a misled agent can do; and "changed upstream
since triage" keeps an edit made after a person looked at the item from
slipping by.

## Where work comes from, and its bounds

- **One log.** `work.events` is the only record. Upstream sync, people's
  dashboard actions and agents' records all append through the tracker's
  validation, and an item's state is a fold.
- **Roots come from upstream sources and people only.** Agents create
  children, and only under an item they currently hold. Every agent-created
  item carries its parent and root ids. Items people create do not draw from
  the root budget.
- **A work budget per root.** Settings, with defaults of at most 12 items in
  the tree, depth at most 3, and a cost cap. Every child, attempt and redo
  draws from it, and budgets only decrease. A create that would exceed the
  budget is refused and logged as refused, and the root goes to Needs you:
  raise the budget, split by hand, or close.
- **What an agent may create live under a held item, within budget:** `split`,
  `augment`, `question` and `test`. Root-shaping changes are proposals. Out-of-scope work, such as a new bug it
  found, is a proposal shown in Triage as "proposed by <agent>". It becomes a
  root only when a person accepts it.
- **Creation is idempotent.** The child's key is the parent plus an
  agent-supplied key, written with `cache_put_if` absent, so a retry never
  duplicates.
- **Proposals are capped per attempt and per agent.** They share the 24-post
  limit with children and prerequisites. An agent may also have 16 proposals
  waiting for a person at once, and 8 from work under one root, over all its
  attempts. A proposal leaves the count when a person accepts or rejects it.
  A prerequisite that files work in another repo and a deferred review
  finding count the same way. The tracker checks this before it appends, so
  the fold of an existing log does not change.
- **Waiting proposals use up the root's budget.** A proposal takes nothing
  from the root's budget, but a root whose waiting proposals number as many as
  its remaining items, or whose cost budget is spent, refuses the next one as
  `over_budget`, so repeated claims cannot flood it. A person's accept or
  reject takes the proposal out of the count, which frees the room; an
  accepted proposal is a root with a budget of its own.
- **Attempts.** A claim that ends in a release, a lapse or a give-up may be
  retried once on the same attempt, so an attempt takes at most two claims.
  After that the item goes to Needs you. A redo starts a new attempt, at most
  three per item, with at most two attempts open at once; a person closes
  one to open another. Every attempt draws its cost from the root, at least a floor for each
  claim end and each ask (see [Cost and spend](#cost-and-spend)).
  Finishing an attempt of a fix leaves it in review, so it can still be
  redone. It closes when a person or upstream sync records the merge or closes it.
- **Closure.** A parent cannot complete while its children are open. Closing
  an item, from upstream or by a person, cancels its open descendants and
  takes them out of Ready. An upstream close as completed or fixed counts as done and frees dependents; as not planned, a duplicate or won't fix (or with no reason) it counts as dropped and sends them to Needs you. A proposal nobody accepted can be rejected.
- **Termination.** Agent-created work is at most (upstream roots + accepted
  roots) x the per-root budget, plus whatever people create. Agents spend
  budget and never create it.

Budgets are enforced by a compare-and-set on the budget key (`cache_put_if`),
not by counters, because Felix has no bounded add (gap 42).

The tracker keeps the text of context, note, question, answer, decision and
triage records in memory, at most 8 MiB, least recently used first. A text it
let go is read back from `work.events` when a call needs it, so the cap costs a
read and never loses text.

### Cost and spend

Agents report what they used, not what it cost. Each report names the model
and counts usage in the unit the agent is billed in. The tracker turns that
into cost with a price table that people keep, so a new model or a price
change needs no change to the log.

**Usage.** `work_complete`, `work_release`, `work_ask` and a `prerequisite`
post take `usage`, a list with one entry per model the attempt used since its
claim or its last ask:

```json
{"usage": [
  {"model": "claude-opus-5-5", "unit": "tokens", "input": 20000, "output": 4000,
   "cache_read": 150000, "cache_write": 8000},
  {"model": "gpt-6", "unit": "aic", "milli": 1500}
]}
```

`tokens` counts input, output, cache-read and cache-write tokens; leave out
what you did not use. `aic` counts AI credits in thousandths, so 1.5 credits is
1500. A report holds at most 16 entries and names each model once. A model name
is up to 100 letters, digits and `.-_:/@`. An entry that used nothing is left
out. `cost_micros` is no longer read: a report that sends one above 0 is refused
with a message that says to send `usage`. An older `kbl-mcp` sends 0, which
passes and is charged the floor, so the tracker can be upgraded first.

**Prices.** A person sets the price of one unit of one model with
`POST /v1/work/prices`, which writes a `price_set` record:

```bash
curl -X POST "$KBL/v1/work/prices" -H "Authorization: Bearer $PERSON_TOKEN" \
  -d '{"model": "claude-opus-5-5", "unit": "input", "price_micros": 15000000}'
```

`unit` is `input`, `output`, `cache_read` or `cache_write`, priced in millionths
of a dollar per million tokens, or `aic`, priced in millionths of a dollar per
credit. The example is $15 per million input tokens. A price takes effect when
it is written, or at `effective_from_ms` if that is later. It cannot reach back:
an `effective_from_ms` before the tracker's clock is refused with `bad_price`,
so leave it out for "now". Prices are never edited: a new `price_set` for the
same model and unit takes over from its time on. The tracker fetches no prices
from vendors.

To change a price without overwriting one someone else just set, send the price
you read as `expected_price`: `{"price_micros": 15000000, "since_ms": 1760000000000}`
(its value and when it took effect). If the price in effect when the new one starts
is not that one, the tracker answers `409` with `why: "price_changed"` and writes
nothing; read the price again and retry. For a first price, send
`"expected_price": {"none": true}`: it is refused the same way if a price is in
effect by then. `none` is its own field and only `true` is accepted, so a price
left out or read wrongly cannot turn into "expect none" by accident. The check and
the write happen together, so of two posts that expected the same price (or both
expected none) only one lands. Leave `expected_price` out and nothing is checked.

**Importing prices.** `kbl-tool prices import` writes `price_set` records from
a price table, so a fresh install is not empty:

```bash
export KBL_TRACKER_TOKEN=...          # a person's token; only a person may set a price
kbl-tool prices import --default      # the table shipped with this build
kbl-tool prices import my-prices.json --dry-run
kbl-tool prices import --default --team alpha     # a token in more than one team names its team (or set KBL_TEAM=alpha)
```

`--default` is `tool/prices/default.json`: Anthropic's list prices, dated
(`as_of`), with `source` saying where they were copied from. A person keeps it
by hand; nothing fetches it. It prices only what the vendor lists, so a model or
unit it does not list (`cache_write`, for one) stays unpriced and is charged the
floor until you add it. Prices are one number per unit, so a vendor's tiers are
not expressed: `claude-haiku-5-5` is listed at its price for prompts up to 100K
tokens ($0.10 input, $0.50 output per million; beyond that $0.50 and $2.50), and
longer prompts are undercharged. Check it against the vendor's pricing page.

A table is JSON: `as_of`, `source`, and `prices`, each with `model`, `unit` and
`price_micros`, and optionally `effective_from` as `YYYY-MM-DD` (UTC). A day that
has not come yet is sent as a price for later. A day that has passed, or is
within ten minutes, means now, because a price cannot reach back and the tool's
clock and the tracker's differ. The import reads `work.events` first and skips a
price that is already in effect (a price for later is compared with what is in
effect on its own day), so running it again writes nothing. A
price that differs from the one in effect is listed and left alone, in case a
person set it on purpose; `--update` writes it.

To follow a vendor's price change, edit the table (or your own copy), run the
import with `--update`, and the new price takes over from that moment. Usage
already reported keeps the price it was charged at. The import needs the log at
[format 9](upgrades.md#formats), since it sends each table's `as_of`; the tracker refuses it below that. If it stops
partway, run it again.

**How a report is charged.** The fold prices each report when it folds it,
with the prices in effect at the time the report was written. A later price
change does not reprice history. The report is charged its priced usage, or
the floor if that is more: `KBL_COST_FLOOR_MICROS`, 100000 ($0.10) by default,
which the tracker writes on the record as `floor_micros`. Usage is the agent's
word, so the floor is what bounds a tree's cost: an agent that reports nothing
still spends its root's budget, and the default $10 budget allows about 100
charges per tree. A lapsed or revoked claim is charged the floor. Set the floor
to 0 to charge only priced usage. The setting is one for the whole tracker
process; with [teams](#teams) each team's tracker charges it against its own
trees and budgets.

**A model with no price.** Usage of a model or unit that has no price is
recorded, not refused, and never guessed. Until a price exists the report is
charged its floor (or its priced usage, if that is more), so work goes on. The
model is listed in `unpriced` in the spend view, so a person sees it needs a
price. When a person sets one that takes effect at once, the fold prices the
waiting usage with it and draws the difference from the root's budget. The dashboard lists these models in a card on Spend and lets a person set a price there; it
is not a Needs-you item. A price set for a later time prices only usage from that time on. At most 32 models may
wait for a price at once; a report that would add another is refused with
`bad_usage` until a person prices some.

**Old prices.** The spend view also lists every price set (`prices`): model, unit,
price, when it starts, and `as_of` when an import recorded the vendor day it copied
it from. The dashboard shows "check pricing" next to a model whose oldest price in
effect is more than 90 days old, counted from `as_of` when there is one and from
the start otherwise (`?price_age_days=N` changes it). It is a hint only.

**Models and prices.** The dashboard's Models and prices page (from Spend, or the
palette) lists every model seen in recorded usage, priced or not, with each unit's
price in effect, when it started, where it came from (an import and its `as_of`
day, or a person), the usage accrued, any price set for later, and the model's
full price history. A signed-in person sets or changes a price there with the same
`POST /v1/work/prices`: a day typed in the form that has not come yet is sent as
`effective_from_ms`, anything else means now. Changing a price in effect shows old
to new and waits for a confirm; usage already charged keeps its price. A price of
0 is accepted, by the import and the page alike, for a model that costs nothing.
The import sends the table's `as_of` with each price (`as_of` on `price_set`, a
real `YYYY-MM-DD` day that a person's own price leaves out). A price with `as_of` is [format 9](upgrades.md#formats); one without stays format 8. Two prices with the same start share one `as_of`: the later record's.

**Budgets.** A root's cost budget is in the same derived cost, in millionths of
a dollar, and is shared by every model and unit in the tree. The budget is
checked when a claim is made, so claims already in flight can still finish and
be charged after it reaches 0.

**Older logs.** All of this is [format 8](upgrades.md#formats). On a log below
it the tracker writes each report as before, with the floor as its
`cost_micros`, and drops the usage. Records from before the raise keep the
`cost_micros` they carry, which counts toward spend and budgets as it always
did.

**Spend.** The charges add up on the item that took the claim, so spend is a
read of the fold, not a separate record. `kbl-tool work spend` folds
`work.events` and prints the total, in millionths of a dollar, per root (the
root and everything under it), per reviewer tier (reviews only) and per repo
(`source:repo` of the item's root, so a review counts where the request it
reads lands). `by_model` gives each model's usage and the cost of its priced
part. The total can be more than their sum: a report charged its floor adds
what the floor came to above its priced usage, and costs reported before
format 8 count only in the total. `unpriced` names each model and unit still waiting for a price. The
wasm fold exposes the same numbers via `Folder.spend_json()`, and the dashboard's
Spend dialog shows them per root, reviewer tier, repo and model.

With input priced at $15 and output at $75 per million tokens, 20000 input
and 4000 output tokens cost $0.30 + $0.30:

```bash
kbl-tool work spend
# {"total_micros": 1400000,
#  "by_root": {"g3f9a1c0d2b7e4a65": 1400000},
#  "by_review_tier": {"deep": 300000, "fast": 100000},
#  "by_repo": {"github:GetFelix/felix": 1400000},
#  "by_model": {"claude-opus-5-5": {"input": 20000, "output": 4000, "cache_read": 0,
#     "cache_write": 0, "aic_milli": 0, "cost_micros": 600000, "unpriced": false}},
#  "unpriced": []}
```

## The dashboard

The dashboard shows everything before and around
an attempt: what waits on you, where work is piling up, and what the team delivered.
It is the page someone opens first in the morning.

It is laid out like the trackers people already use, so nobody has to learn it.
[usability-test.md](usability-test.md) is the test that checks this with five
people who use Linear or Jira every day.

### Finding your way around

- **Sidebar.** My queue with its count, All work, Metrics, then saved views
  and one entry per source. `[` hides it.
- **List and board.** All work has two layouts of the same items. The board
  shows the stages as columns. The list shows one item per line, grouped by
  stage, tier or source. `Ctrl+B` switches, and a saved view remembers its
  layout.
- **Side panel.** Selecting an item opens it on the right: status, tier,
  assignee and source at the top, then what the item needs and its choices.
  `Esc` closes it.
- **What is happening.** Under the title the panel leads with a **Now**
  sentence: who holds the item, the attempt, how long ago it last wrote and
  the claim-time deadline while it is still ahead (renewals are not recorded), or why it is not moving and who must act. Then
  the actions (accept, tier, take, assign, steer, close), **Linked work** (the
  pull request with its checks, child work such as triage with the answer it
  recorded, what the item waits on and what it holds back), **Activity**
  (the log's records on the item, newest first, with relative times that keep
  moving: claims, notes, each step with its exit code and a folded tail of its
  output, decisions, edits, questions and answers, and how each attempt ended)
  and **Attempts** (how each ended, what it handed off, whether a person
  marked the summary accurate, what it cost, and a link into the Story at that
  attempt's first decision). All of it is read from records the fold already
  applies; nothing is added to the log. A cost is what the fold charged the
  item at that record, so usage with no price yet shows as $0.00 until a price
  arrives. The claim deadline is the one the claim was written with; the fold
  does not see an agent's heartbeats.
- **Story.** An item with a recorded decision shows its story in the side
  panel: the decisions in order across attempts and people, grouped under
  Understand, Reproduce, Cause, Fix and Verify. Each decision shows what the
  agent recorded (the choice, why, so far, the revert check) with the offsets
  of the steps it cites and the edits that carry it out, each one a link to
  the record it names. Flags above it are rules over those records, listed in
  [decision-replay.md](decision-replay.md#claims-checked-against-steps). The
  view reads only recorded fields and generates nothing.
- **Command palette.** `Ctrl+K` (`Cmd+K` on a Mac) lists every action for the
  open item and every place to go, and finds items by number or title. Anything
  that has no key of its own is here.
- **Search and filters.** `/` searches the view. `f` filters by status, tier,
  source, agent, or "waiting on me". Filters show as chips, and a set of
  filters can be saved as a view in the sidebar.
- **Selecting several.** `x` ticks a row and `Shift+j` ticks more. A bar
  appears with the actions the ticked items share, and one action applies to
  all of them.
- **Undo.** Every action changes the view at once and shows a toast with
  Undo (`Ctrl+Z`) for six seconds. If the tracker refuses the write, the
  change is put back and the item says what happened and what to do.
- **Addresses.** The view, the open item and the filters are in the URL, so a
  link opens the same thing for someone else.
- **Loading and empty.** The first read shows placeholder rows in the shape
  of the list. An empty queue says so and points at the board. Filters that
  match nothing offer to clear themselves.
- **Light, dark and density.** Under Display: two themes and comfortable or
  compact rows.

My queue is the only place things arrive. Nothing pops up when a record lands.

### My queue

The landing view. It lists everything that waits on the person looking, which
leaves out items assigned to someone else:

- a question an agent asked, or an item triage found unclear. The card shows
  the options as numbers with the default marked, and a field for any other answer,
- a failure that needs a decision,
- a triage proposal to accept,
- a PR to review, or an approved one to merge.

Items are ordered by how long they have waited, oldest first, with tier as the
tie-break. The Needs you view of the dashboard sorts by the log offset at which
the item began waiting for its present reason, which the fold records on the
item (`waiting_since`), so the order is the same on every page load and after a
restart; items that began waiting at the same record fall back to their id. The count is in the tab title, so it shows without opening the page.
Snoozed and parked items drop out of the count, and come back when the snooze ends or the item waits for a new reason.

Every card is keyboard first, and the keys mean what they mean in Linear,
GitHub and Jira. `?` shows the map in the page.

| Key | What it does | Records |
|---|---|---|
| `Ctrl+K` | Command palette: every action and every place, by name | nothing |
| `/` | Search the view | nothing |
| `?` | Show the key map | nothing |
| `j` `k`, or the arrows | Move | nothing |
| `Enter` | Open the item's story, or go to its note field | nothing |
| `o` | Open the diff or pull request | nothing |
| `x`, `Shift+j`, `Shift+k`, `Ctrl+A` | Select a row, select more, select all | nothing |
| `Ctrl+Enter` | The card's main choice | what that choice records |
| `1` to `9` | The card's numbered choices (table below) | what that choice records |
| `s` | Set status. Lists only the moves a person can make from here, such as snooze, park, close, retry or mark merged | the record of that move |
| `p` | Set tier (on a proposal: edit it before accepting) | `tier_changed` |
| `a` | Assign to yourself or a person of the team, so no agent claims it, or to nobody. Agents come later ([#324](https://github.com/GetFelix/KBL/issues/324)) | `taken`, `reassigned` |
| `h` | Snooze for four hours | `snoozed` |
| `c` | New item, filed in one of the trackers | `imported`, when ingest sees it |
| `f` | Filter | nothing |
| `g` then `i`, `l`, `b`, `m` | Go to My queue, the list, the board, Metrics | nothing |
| `Ctrl+B` | Switch between list and board | nothing |
| `[` | Show or hide the sidebar | nothing |
| `Ctrl+Z` | Undo the last action | takes its records back |
| `Esc` | Close the panel, clear the selection, go back | nothing |

On a Mac, `Ctrl` is `Cmd`.

What only KBL has is not on a letter. Each card shows its own choices as
numbers, so they need no memorising. `Ctrl+Enter` takes the card's main choice: the
first, except on a question, where it sends the default (below). It works from
the note box, not from search or other text fields:

| Card | `1` | `2` | `3` | `4` |
|---|---|---|---|---|
| Triage proposal | Accept: `triaged`, `linked` | Merge into the item it duplicates: `rejected` with the original | Decline: `rejected` | |
| Question | The first option: `answered` | The second option | and so on | The number after the last option goes to the free text field |
| Failure | Retry, with the note if one is typed: `retried`. The tracker refuses a retry after a failed check without a note | Close: `rejected` | | |
| Redo limit reached | Take it yourself: `taken` | Split it: `split` | Close: `rejected` | |
| Blocked | Mark the dependency resolved: `unblocked` | | | |
| Held by an agent | Send the note: `steered` | Stop it: `released` | | |
| Held by you | Release: `released` | | | |
| PR with no reviewer | Ask a person to review it, picked from a list: `review_requested` | Approve: `approved` | Send back with changes: `changes_requested` | Reject: `rejected` |
| PR waiting on you | Approve: `approved` | Send back with changes: `changes_requested` | Reject: `rejected` | |
| PR approved, not merged | Record a merge done upstream: `merge_marked` | Reject: `rejected` | | |

On a question, `Ctrl+Enter` sends what is typed in the free text field, or the
default option if nothing is typed. Marking the agent's summary accurate or
inaccurate (`summary_rated`) and parking an item are buttons on the card and
entries on the palette. In the live dashboard the summary marks are two buttons
per finished attempt in the Story section of the item's panel, reachable with
Tab; they write the record and show the mark once the log carries it.

The letters changed to match other trackers. Someone who knew the old map can
read this once:

| Was | Now |
|---|---|
| `x` reassign | `a` assigns. `x` selects |
| `c` send a PR back | A numbered choice on the PR card. `c` creates |
| `s` snooze | `h` snoozes. `s` sets status |
| `a` accept, approve, answer | `Ctrl+Enter`, or `1`. `a` assigns |
| `t` take it | `a`, then Me |
| `d` reject or close | A numbered choice, and Closed under `s` |
| `g` merge a duplicate | `2` on a triage card. `g` starts a go-to |
| `r` retry or stop, `u` unblock, `v` request a reviewer, `m` record a merge | Numbered choices on their cards |
| `n` note | `Enter` goes to the note field |
| `z` park | `s`, then Parked |
| `+` `-` rate the summary | Buttons on the card, and the palette |
| `y` story | `Enter` |
| `1` `2` `3` views | `g` then `i`, `l`, `b` or `m` |

In the style frame these actions are local. Each one appends a `work.events`
record to a list on the page and the view folds again, so the board is still
a fold of the log and the list shows exactly what would be written. Undo takes
the records back out. Live, the same actions go through the tracker's REST API
and MCP tools. The history is append-only, so the plan for the live
dashboard is to show the change at once and send the write when the undo
toast closes, or at once if the page is closed first.

### Board

The stages, left to right. An item sits in exactly one and moves right as
`work.events` records it.

- **Triage** holds items nobody has accepted yet. An agent that completed the
  item's triage work has attached a proposal: a tier with one line of why,
  dependencies, and a possible duplicate, including one from another tracker.
  You accept, edit or reject it. An item whose triage work nobody has claimed
  yet says so.
- **Needs you** groups failures by cause (tests red, timeout, claim lost, gave
  up) under the agent's last message, next to questions.
- **Blocked** leads with the root blockers: the unfinished items that hold the
  rest, each with "holds up N". Selecting any item draws its dependency chain
  across the columns.
- **Ready** is grouped by tier.
- **Held** names the agent and links to its run. You can stop it, send it a
  note, reassign it or take the item.
- **In review** is an agent PR waiting on a person. A card shows the PR, its
  size (lines and files), CI state, review rounds, who it waits on and for how
  long. Two states have their own badge and a one-click action on the card:
  "CI green, no reviewer" (Request review) and "approved, not merged" (Open to
  merge). Both come
  from the `pr_status` ingest records. A failed required check moves the item
  to Needs you with the check's name and a link, unless an agent holds it. A
  redo from there needs a note written since the failure, and from format 4
  ([upgrades](upgrades.md#formats)) it is handed out while the check is
  still red once the item has a note newer than the failure; otherwise it
  waits for a note or for green. The tracker
  never merges. It records the merge it sees. A PR also carries the agent's
  handoff: what it changed, what it checked, what it is unsure of. The
  reviewer can mark the summary accurate or not.
- **Done** is the last day's merges.

Each column shows its count, its oldest item, and the team's own p50 and p85
for that stage. An age turns amber past the p50 and red past the p85 of
finished visits to that stage. Until a stage has five of them, a fixed
fallback is used and the column says it has no history.

Where the items come from is a chip in the header: arrivals in the last day,
and per source on click.

### The review limit

Agents open PRs faster than people review them, and a claim that only adds to
the review pile makes every PR wait longer. So the tracker has a limit on
agent fixes headed for review. At the limit agents stop claiming new fixes.
Triage, answers and reviews carry on, and the Ready column explains why nothing
is moving.

The limit is `KBL_REVIEW_LIMIT` (or `--review-limit`), 3 by default, and 0
turns it off. It counts two things: items in In review (fixes an agent
finished that nobody has closed) and fixes agents hold now that have not
reached In review yet. Counting the held ones keeps a crowd of agents from all
landing in review together. When that sum reaches the limit, `work_next` skips
Ready fixes. It is soft only in that it never blocks triage, questions,
reviews, resumed work or finding fixes. A finding fix is one a review asked for
on a PR. It closes without a review of its own and the PR waits for it, so
pausing it would stop that PR from ever leaving In review. The paused fixes
come back on their own once a merge, a close or a finished hold brings the sum
under the limit. Work an agent already holds is not taken back.

The header says "review full: agents paused, 3/3: 2 in review, 1 in progress"
when the review load is at the limit. The page reads that load from the same
fold function the pause uses, so the two cannot disagree: In review plus the
fixes agents hold, split into its two parts.

There is no evidence behind the default of 3: the research found nothing that
says how many open PRs a reviewer can handle, and "two or three per active
reviewer" was a guess. Pick a number, watch the review column, and change it.

`GET /v1/me` carries each team's limit as `review_limit` (null while the team
is not serving). The dashboard reads it once you sign in, so a viewer who has not signed in sees no limit in the header.

### The footer names the constraint

One sentence names the stage that is slowest to move and why, for example
"review: 6 PRs waiting, oldest 31h". It picks the stage whose oldest item is
furthest past its own p85, and it never blames agents in general. Idle agents
with work ready is reported as a claim path problem, since that is a defect
and not a lack of capacity. The in and out rates sit beside it.

### Metrics

The third tab is for the team and the flow. It has no per-person number, and
none will be added: ranked individuals turn a measure into a target. Bot
approvals are never counted as review.

- Time in each stage, p50 and p85. The dashboard shows it, with throughput,
  rework, cost per done item and the supervision numbers, from the fold alone;
  see [Flow metrics](flow-metrics.md).
- Time waiting on a human, per merged fix.
- Time to the first human action on My queue and on In review. The tracker
  serves it today, see [Time to first human action](#time-to-first-human-action);
  the dashboard does not draw it yet.
- Trust in summaries: the share of marked summaries a person marked accurate.
  The dashboard's Metrics dialog shows it, see [Summary trust](#summary-trust).
- Throughput against arrivals, and a cumulative flow chart. A band that widens
  is the constraint.
- Blocked time and the items that hold the most up.
- Rework: fixes that needed a second attempt or a second review round,
  reverts within 14 days, and merges after human edits.
- Merged against rejected, by task class.
- Cost per merged fix, with failed attempts counted against it, and human
  minutes per merged fix. Minutes are an estimate.

These come from `work.events` alone. Cost and minutes arrive as `cost` and
`human` records. The numbers are published as they are, not picked.

#### Time to first human action

How long an item waits on a person before a person writes anything about it.
The fold derives it from records that already exist; no new record is written
and no model reads anything.

An item starts waiting when it enters one of these:

- an agent's proposal or triage answer waits to be accepted (`proposal`)
- an agent's question, or triage that found the issue unclear (`question`)
- a failed check, or an attempt that used its tries (`failure`)
- the work is done and its pull request is in review (`review`)
- any other Needs you reason, such as over budget or flagged (`other`)

The wait ends at the first record whose actor is a person on that item: an
answer, an accept, a note, a close. Agent and tracker records do not end it.
A snooze or a park does not either: it only hides the card, so it is not an
action. A wait is one unbroken stretch. If the item stops waiting and later
waits again, that is a second wait and counts twice; a second person record
during the same stretch does not count again. An item that stops waiting
without a person acting (a green check, a close by upstream sync) leaves no
wait at all.

`GET /v1/work/metrics/time-to-human` counts a wait in the window its ending
record falls in. Waits still open are reported as `open`, with the time the
longest began, and are left out of p50 and p90. Percentiles use the nearest
rank. The numbers are for the team whose log is folded; nothing names who
acted. The fold keeps the last 20,000 finished waits; past that the response sets `truncated` for a window that reaches the dropped ones. The waits are not in a state's wire form, so a state restored from that counts only from the first record after it, and `truncated` is true for any window reaching earlier.

#### Handoff beside a finished attempt

The dashboard shows what an attempt handed off next to the item, in the panel and in the Needs you inbox, so
a person can merge, steer or reject without opening the pull request, the log and the checks in turn. It
reads the latest attempt that finished (an accepted `done`) and shows only what the fold took from the log:

- **Summary.** The attempt's last `progress` note, as written. The note is stored inline; one kept in an artifact is not shown.
- **Diff.** Shown as "Logged edits: ..." with a note that it is not the pull request's diff. Files touched and lines added and removed, counted from the `+` and `-` lines of the attempt's
  `edit` hunks. A hunk stored as an artifact is not counted, and the page says "at least". The pull
  request's own diff stats are not recorded, so the figure is what the agent logged, which may differ from the pull request.
- **Pull request and checks.** The item's `pr` as a link, with the checks and the failing check's name from the
  last `pr_status`.
- **Failed steps.** The attempt's `step` records with a nonzero exit code (the first ten, with a count of the rest).
- **Mark.** The standing `summary_rated` mark, and Accurate and Not accurate buttons that write one.

A record the fold refused (a `done` with its own cost on a log at format 8 or above, a mark on a log below 11) counts for
none of it. This is read in the page's fold (`handoffs_json`); no new record kind is added and the format does not change.

#### Summary trust

How often a person who marked an agent's summary marked it accurate. The fold
keeps each finished attempt's latest `summary_rated` mark and when it was
written ([review actions](#review-actions-and-summary-marks)); no new record is
written and no model reads anything.

`GET /v1/work/metrics/summary-trust` counts an attempt in the window its
standing mark was written in. Marking an attempt again moves it to the later
mark's window and its new answer, so an attempt is never counted twice and the
first mark does not linger. A repeat of the standing mark is refused and moves
nothing. Each attempt counts, so a redo's summary is marked and counted apart
from the first attempt's.

- `rated`, `accurate` and `not_accurate` are the marks in the window;
  `accurate_permille` is `accurate` over `rated` in thousandths, and null when
  nothing was rated, so an empty window is never read as 0%.
- `unrated` is the finished attempts nobody has marked as of now. It does not
  follow the window: an attempt has no finish time in the fold, so it is the
  backlog still waiting for a mark.
- `min_sample` is 10. Below it the dashboard shows the counts first, the share
  muted, and says there are too few marks to read a trend from.
- `undated` counts marks a state decoded from its wire form has no time for; they are left out of the
  window. A tracker folding the log reports 0.

The numbers are for the team. There is no breakdown by agent, model or
reviewer, and none will be added: the fold keeps no agent or model per attempt
for a summary, and a per-reviewer figure ranks people, which the Metrics tab
does not do. A team with one reviewer still reveals that person's marks; the
route is for people of the team, who can read the same marks on each item.

[dashboard-research.md](dashboard-research.md) says why each part is there and
how much evidence stands behind it.

The look is fixed in a style frame before anything talks to Felix: `prototype/intake-frame.html`
folds a hand-written `work.events` fixture (three trackers, a blocked chain, items
waiting on a person, held items, PRs in review, two weeks of finished work), a
copy with the review limit reached, and a generated burst of 320 items.
`node scripts/check-intake.mjs` checks them, including that My queue matches
what waits on the viewer, that actions append records, and that the keys above
do what the table says.

**Look.** KBL amber (`#C86A1E` dark, `#A4520F` light, with a lighter step for text on dark
cards) is work in motion and the current selection. A yellower amber (`#FFD04D` dark,
`#6E5F00` light) is for trouble the system handled. In charts that colour means a person
and slate blue means agents. Red is only for a refusal, so red always means
Felix said no. Shapes carry meaning before colour does, so the page reads in greyscale. Inter is for names and labels and
JetBrains Mono for ids, offsets, versions, counts and money. It is dark by default
with a light theme, and `prefers-reduced-motion` turns movement into a single flash.
Rows come in two densities. The layout has no novelties: a sidebar, a list or
a board, and a panel.

![My queue, with a question open](screenshots/ux/queue-dark.png)

![All work as a list, light theme](screenshots/ux/list-light.png)

![All work as a board](screenshots/ux/board-dark.png)

![The command palette](screenshots/ux/palette-dark.png)

![Several items selected, and the undo toast](screenshots/ux/bulk-undo-dark.png)

![Pull requests waiting for review](screenshots/ux/review-states-light.png)

![Metrics](screenshots/dashboard-metrics.png)

`docs/screenshots/ux/` has every view and state in both themes.
`node scripts/shoot-intake.mjs docs/screenshots` takes them again.

## Where Felix stops

- **Queries over the graph live in the tracker's fold.** "What does this
  block", "everything waiting on a person" and the board's filters are
  answered from the tracker's in-memory projection of `work.events`, rebuilt
  on start. That is fine for thousands of items. If the queue needs ad hoc
  queries or grows past what a fold rebuilds quickly, the projection moves to
  Postgres and `work.events` stays the record.
- **Dependency cycles are the tracker's check.** `work_link` refuses an edge
  that closes a cycle. Felix has no view of the graph.

## Risks

- **The tracker becomes a dependency of the daily bug loop.** Felix is alpha,
  and 0.8 replaces its replication engine. Run the tracker on a single broker
  until 0.8 settles, and keep an export of `work.events`, so an outage stops
  the board but loses nothing.
- **Gaps it hits.** Gap 1 (max attempts is broker-wide) matters more here: an
  item that keeps failing should go to a person after two attempts. Gap 8
  means the tracker, like `kbl-mcp`, is Rust. Gaps 5 to 7 limit the viewer
  as they do for runs.

## Order

M2 and M3 in the [build order](design.md#build-order): GitHub ingest,
`work_next` and the ready tiers, then triage and augmentation as work kinds and dependencies, then Claude Code
working `agent-ok` issues. The dashboard goes live in M4. ADO and Jira ingest come
after GitHub.

The first source is GetFelix/felix. Only issues labelled `agent-ok` are
imported as claimable, and replication and storage safety work stays out. An
agent works in its own worktree and opens a pull request, which goes through
the same review and CI as any other. The tracker never merges.
