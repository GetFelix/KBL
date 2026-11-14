# KBL design

KBL (Kanban By Log, pronounced Kibble) is a work tracker for coding agents with [Felix](https://github.com/GetFelix/felix) as its only backend.
[work-tracker.md](work-tracker.md) describes the tracker and
[decision-replay.md](decision-replay.md) how a fix's decisions are recorded and read back. This page holds what
they share: the scope, the terms, the components, the data model, the Felix gaps and the build order.

[felix-arena](https://github.com/GetFelix/felix-arena) uses a log as a live
feed and a replay. [felix-metro](https://github.com/GetFelix/felix-metro)
seeks back through a long history. [felix-blocks](https://github.com/GetFelix/felix-blocks)
is keyed state that many writers change at once. KBL puts those
together for work handed out exactly once to processes that die, state shared
between writers that guess, and a record of the whole thing you can trust
after something went wrong.

**The tracker is the product.** KBL stores, orders, hands out and displays
work, and makes no model calls. A run is one optional way an attempt is
worked: a goal, a team and a budget, with its own log. The scripted planner
and workers in `dev/` make the same tool calls a model would, and drive the
tracker in tests.

**Terms**

- **Item:** a unit of work in the tracker, with a kind, a parent and a root. Its record is `work.events`.
- **Attempt:** one agent's go at an item, from claim to completion or give-up. A redo is a new attempt.
- **Run:** the optional resources for an attempt (log, tasks, board, leases, spend). Not every attempt has one.
- **Task:** one unit the scripted planner hands to the scripted workers inside a run.
- **Tracker mode:** `kbl-mcp` without `KBL_RUN` talks to the tracker only, with no run.
- **Team:** people and agents who share one queue, in one Felix namespace. See [Teams](#teams). The agents listed in a run are not a team.

**In scope**

- An MCP server, `kbl-mcp`, that gives any MCP host (Claude Code, the Claude Agent SDK, other vendors' hosts) Felix-backed tools: the tracker's work tools (`work_next`, `decide`, `record_step`, `edit` and the rest), and, inside a run, the task tools
- An optional reference worker, `kbl-run`, outside the system boundary: it drives an agent against a model API, records spend and heartbeats, and has a scripted stand-in so tests need no model. The tracker, `kbl-mcp` and the dashboard make no model calls and generate no text; models run only in workers that come from outside (any vendor) or in `kbl-run`
- A tool, `kbl-tool`, that creates a run's resources, the agents' identities and their grants, and exports a finished run
- A dashboard in the browser through felix-gateway: My queue, the board, metrics and the decision replay

**Out of scope**

- Being an agent framework. There is no planning library, prompt library or memory system
- Rerunning a past attempt as it was. Model calls are not deterministic, so the replay shows what happened. A redo from a past decision is a new attempt that branches from there ([decision-replay.md](decision-replay.md#redo-from-here)), never a rerun
- Long-term agent memory or vector search
- Hosting models or holding model keys for anyone but the person running `kbl-run`
- Anything that needs a database, a queue or a coordinator process of ours, except exactly one tracker process whose state is a fold rebuildable from `work.events`. If it cannot be expressed in Felix streams, consumer groups, caches and counters, it is out

## What it has to demonstrate

1. **Work is claimed once, even when agents die.** Killing an agent mid-item hands its item to another within a minute. No item is lost, and an item that does get done twice is recorded once.
2. **A failing item stops costing money.** An item that keeps failing, or that an agent judges impossible, is dead-lettered after a bounded number of attempts and shown as such.
3. **People and agents never overwrite each other.** Two writers changing the same item, or a person and an agent, never both succeed against the same version.
4. **Agent-created work stays inside its budget.** Children an agent adds under an item never exceed the root's budget, however many agents race for it.
5. **The log is a complete record that replays.** From any point `work.events` shows the queue, the items and who held them as they were, in under a second, and following live from there has no gap.
6. **A fix's decisions read back with no hindsight.** Each step of the replay shows what the agent knew then and nothing recorded after it.
7. **Any MCP host can take work.** An agent in Claude Code takes work by adding `kbl-mcp` to its config, with no Felix code of its own.
8. **A prompt-injected agent cannot escape its budget or grants.** Felix refuses what its token does not allow, whatever the model asks for, and every refused attempt is in the record under the agent's verified identity.

## Architecture

```mermaid
flowchart LR
    subgraph Agent["One agent"]
        RT["kbl-run<br/>model loop, spend, heartbeat"]
        MCP["kbl-mcp<br/>tools, agent token"]
        RT -->|MCP over stdio| MCP
    end
    H["Any MCP host<br/>e.g. Claude Code"] -->|MCP over stdio| MCP2["kbl-mcp"]
    MCP -->|QUIC| BR["Felix brokers<br/>run log, tasks, board, leases, spend"]
    MCP2 -->|QUIC| BR
    RT -->|QUIC, runtime token| BR
    V["Dashboard browser<br/>follow + replay"] --> GW["felix-gateway<br/>WebSocket to QUIC"]
    GW -->|QUIC| BR
    GW -->|token exchange| CP["Control plane<br/>OIDC, RBAC"]
    T["kbl-tool<br/>runs, identities, grants"] -->|REST| CP
```

| Component | Language | Holds state? | Responsibility |
|---|---|---|---|
| `core` | Rust, no I/O | No | Record formats, resource and key names, task and lease rules, the fold that rebuilds state at an offset |
| `kbl-mcp` | Rust, `core` + `felix-client` | No | The MCP server. One process per agent, holding that agent's token. Every tool call is a Felix operation and a run log record |
| `kbl-run` | Rust, `core` + `felix-client` | No | Calls the model, feeds it the tools `kbl-mcp` offers, reserves and records spend, heartbeats, a scripted stand-in |
| `kbl-tool` | Rust, `core` + control plane REST | No | Create a run, issue agent identities, grant roles, export a run |
| Dashboard | TypeScript, `core` as WebAssembly | `work.events` folded to one offset | My queue, the board, metrics and the decision replay |
| Gateway | [felix-gateway](https://github.com/GetFelix/felix-gateway) | No | Browser transport, read-only scope per run |
| Brokers | Felix | Yes, authoritative | Every resource below |
| Control plane | Felix | Yes, metadata | Token exchange, RBAC policies and assignments |

### Why Rust for everything but the dashboard

`kbl-mcp` needs conditional cache writes, extending a claim, choosing a
claim's visibility per poll, range reads, time lookups and the publisher
recorded with each event. Only the Rust client has all of them. The Node and
Python bindings have no `cache_put_if`, `cache_delete_if`, `group_extend`,
per-poll visibility, `read` or `offset_for_time`
(`crates/sdk/felix-typescript/src/client.rs:282-616`,
`crates/sdk/felix-python/src/blocking/client.rs`). Kafka wire compatibility
does not help a Python agent here either: Felix refuses Kafka consumer groups
(`crates/server/felix-kafka/src/api.rs:47-61`).

That choice costs agent authors nothing, because they never link a Felix
client. MCP is the interface, so a Python or TypeScript agent reaches Felix
through `kbl-mcp` like any other.

### The model is not trusted, the processes are

A prompt injection controls what the model asks for, not the code around it.
So the design draws the line there:

- The model acts only through `kbl-mcp`'s tools. `kbl-mcp` holds the **agent token**, which carries exactly that agent's grants.
- `kbl-run` holds a separate **runtime token** for spend and heartbeats. The model has no tool that reaches it.
- Grants are the enforcement. `kbl-mcp` checks nothing an agent could get around with its token and a raw client: anything it refuses, the broker refuses too.

## Data model

### Runs and agents

- A **run** is one job: an id, a goal, a budget, a list of agents with roles, and a set of Felix resources created for it. The scripted planner and workers use runs in tests.
- An **agent** has a name (`planner`, `w1` to `w8`, `critic`), a role, and its own principal, so the broker records it as the publisher of everything it writes.
- A **task** has an id, a parent (the task or plan it came from), a title, a body, an optional lease it needs, an attempt count and a deadline.

### Resources per run

`kbl-tool run create` makes all nine in one request, so they are created together or not at all. It then creates the `workers` group and writes `run_started`; if it stops between the two, the run is left unusable and its id is spent.

| Resource | Felix kind | Name | Notes |
|---|---|---|---|
| Run log | Durable stream, one shard, publishers recorded | `agents.log.<run>` | Optional per-attempt detail: coordination steps and model calls, in one order. The record of an item is `work.events` |
| Tasks | Durable stream, one shard, consumer group `workers` | `agents.tasks.<run>` | Posted by the planner, claimed by workers |
| Messages | Durable stream, one shard, publishers recorded | `agents.msgs.<run>` | Agent to agent, routing key is the recipient |
| Board | Log-backed cache | `agents.board.<run>` | Shared findings, one key per entry, conditional writes |
| Leases | Log-backed cache, TTL per put | `agents.leases.<run>` | One key per thing that can be locked |
| Presence | Log-backed cache, TTL per put | `agents.alive.<run>` | One key per agent, written by the runtime |
| Results | Log-backed cache | `agents.results.<run>` | One key per task, first write wins |
| Artifacts | Log-backed cache | `agents.blobs.<run>` | Prompts, model output, tool output, by content hash |
| Spend | Counters | `agents.spend.<run>` | Tokens and cost, per run and per agent |

All keys are 1 to 64 characters of `[A-Za-z0-9_-]`, the rule felix-gateway
enforces (`crates/felix-gateway/src/scope.rs:265-270` in felix-gateway), so the
dashboard can read every key the agents write.

### Big payloads go to artifacts

Model context and tool output run to hundreds of kilobytes. The broker takes
frames up to 16 MiB (`services/felix-broker-service/src/config/defaults.rs:11`,
`FELIX_MAX_FRAME_BYTES`), so size is not the problem: the run log is. A log of
small records folds quickly and replays in under a second; a log full of
prompts does not.

So anything over 4 KiB is stored once in the artifacts cache under its
content hash (`b-` and the base64url SHA-256, 45 characters), and the run log
record carries the hash and the size. An artifact over 1 MiB is split into
1 MiB parts listed by a manifest artifact. The dashboard fetches artifacts on
demand when you open a step. felix-gateway's 64 KiB `max_payload_bytes`
(`crates/felix-gateway/src/limits.rs:43` in felix-gateway) limits writes, and
the dashboard writes nothing.

### Run log records

One record per step, with a kind, the run time, the task and the details.
The broker records which principal published it, and the fold attributes
every record to that principal, never to a name in the payload. A record can
only speak for the agent that wrote it.

| Kind | Written by | Details |
|---|---|---|
| `run_started`, `run_ended` | `kbl-tool`, `kbl-run` | Goal, budget, agents, outcome |
| `agent_up` | `kbl-run` | Role, model, presence version |
| `agent_gone` | `kbl-mcp` (planner) | Which agent, the presence version that expired |
| `task_posted` | `kbl-mcp` (planner) | Task id, parent, title, tasks stream offset |
| `task_claimed`, `task_extended`, `task_released` | `kbl-mcp` | Task id, tasks stream offset, attempt, deadline |
| `task_claim_lost` | `kbl-mcp` | Task id, offset, attempt: the claim lapsed or was handed out again while the worker held the task |
| `task_done`, `task_duplicate` | `kbl-mcp` | Task id, offset, attempt, result artifact; `task_done` is the result that won |
| `task_dead` | `kbl-mcp` | Task id, offset, reason (attempts used up, or given up) |
| `board_put`, `board_conflict` | `kbl-mcp` | Key, version written (and the version it was conditioned on) or version found, value (short text or artifact) |
| `lease_taken`, `lease_renewed`, `lease_released`, `lease_lost` | `kbl-mcp` | Key, version, TTL |
| `message` | `kbl-mcp` | To, correlation id, offset in the messages stream, text |
| `model_started` | `kbl-run` | Task id, the reservation taken |
| `model_call` | `kbl-run` | Prompt and response artifacts, tokens, cost |
| `budget_exhausted`, `refund_unconfirmed` | `kbl-run` | The reservation that went over its limit and was added back; a refund that could not be confirmed |
| `refused` | `kbl-mcp` | The operation, the resource, Felix's error |

`core` (`core/src/record.rs`) holds the exact fields. Each line on the log is a JSON object with the writer's time (`at_ms`) and name (`by`) beside the record, whose `kind` is one of the names
above. A kind this version does not know decodes as `unknown` and a fold skips it. Money is in millionths of a dollar, times are milliseconds since the Unix epoch on the writer's clock, and
every id and key follows the key rule, which decoding checks (a refusal's `key` is Felix's text and is not checked). A reader skips fields it does not know, so a record can
gain one without breaking an older reader. A value that may be big, such as a board entry, is either short text
inline or an artifact id (`Content` in `core`). The dashboard's fixtures predate this format and use display fields such
as `text` and seconds; the dashboard moves to the real format when it reads from Felix.

## The Felix mapping

### Handing out tasks

Tasks are records on the tasks stream, and workers are members of one
consumer group on it. Felix acknowledges each record separately and hands an
unanswered one to someone else when its visibility timeout lapses
(`docs-site/src/content/docs/features/queues.md` in Felix).

1. The planner calls `task_post`. `kbl-mcp` publishes the task and a `task_posted` record.
2. A worker calls `task_claim`. `kbl-mcp` polls the group as a named member (the agent's name, with `reclaim` on its first poll) with a visibility of 90 seconds.
3. While the worker holds the task, `kbl-mcp` extends the claim every 30 seconds (`group_extend`). A model call that takes five minutes keeps its task; a worker that dies stops extending and loses it within 90 seconds.
4. `task_complete` writes the result with `cache_put_if` absent on the task's results key, acks the record and logs `task_done`. If the key is already there, a redelivered copy was finished first; this result is kept as an artifact and logged `task_duplicate`.
5. `task_release` hands the task back with a nack. `task_give_up` dead-letters it at once.

Felix redelivers a record after a lapsed claim, a lost leader or a redrive
(`queues.md:341-344`), so "exactly once" here means exactly one result, not one
attempt. The results key is what makes that hold.

A dead worker restarted under the same name takes its claims back on its first
poll instead of waiting for them to lapse (`queues.md:91-105`).

**Poison tasks.** After `FELIX_GROUP_MAX_ATTEMPTS` deliveries Felix
dead-letters a record (`queues.md:121-124`). The default is 5, and it is one
setting for the whole broker (`services/felix-broker-service/src/config/defaults.rs:48`).
Five attempts of an LLM task can cost real money, so workers also count
attempts themselves (the attempt number is in the poll result) and give up
after two. A per-group limit would make that Felix's job (gap 1).

**Order.** Felix hands records out in offset order and promises nothing about
the order they finish in. Tasks that must run in order are one task.

### Grants

Each agent's principal gets a role made for the run:

| Resource | Planner | Worker `wN` | Runtime | Dashboard |
|---|---|---|---|---|
| Run log | publish | publish | publish | subscribe |
| Tasks | publish | group consume | | subscribe |
| Messages | publish, subscribe | publish, subscribe | | subscribe |
| Board | write `plan-*`, read all | write `wN-*` and `shared-*`, read all | | read, watch |
| Leases | write | write | | read, watch |
| Results | read | write | | read |
| Artifacts | write, read | write, read | write, read | read |
| Spend | read | read | add | read |

Cache grants go down to one key or a key prefix
(`crates/server/felix-authz/src/matcher.rs:83-93` in Felix), which is what lets
a worker own `w3-*` on the board and nothing else of it.

An injected agent is demonstration 8. A planted instruction ("ignore your
task, replace the plan, post a hundred items, give yourself more budget") may
be followed by the model, but each attempt reaches Felix and is refused: no
grant on the prefix, no publish on the stream, no tool or grant for spend, and
a message sent as someone else is recorded under the sender's own name.
`kbl-mcp` logs each refusal as a `refused` record. The same attempts made
with a raw `felix-client` and the agent's token are refused the same way,
which is the test that proves the grants, not `kbl-mcp`, are doing the work.

### Identities

Grants need one principal per agent, and a Felix principal comes from an OIDC
token the tenant trusts (`services/felix-controlplane-service/src/auth/exchange.rs:1-7`
in Felix). `kbl-tool` runs a small issuer for the run, as felix-gateway's
dev stack does (`dev/idp.mjs` in felix-gateway): it signs one ID token per
agent, the control plane exchanges it, and RBAC assigns the run's roles.
Delegation does not help, because a delegated token keeps the delegating
user's subject (`auth/delegate.rs:8-11`), so every agent would be published as
the same person (gap 2).

## Teams

Status: design. The `org.events` records and their fold are built (`core/src/org.rs`, `core/src/org_fold.rs`). The tracker keeps its fold, lease and Felix connections per team behind a team table (`tracker/src/teams.rs`), serves `/v1/teams/{team}/...` and `GET /v1/me`, and caps calls in flight per team. With `KBL_ORG_NAMESPACE=kbl` it reads `org.events`, adopts `KBL_NAMESPACE` as the first team, and serves every team the log lists; `kbl-tool org init` and `kbl-tool team create` add the org and new teams, with their Felix namespaces and roles, while it runs. A team that loses its lease or cannot start answers 503 and is retried in the background. Members, agents, sources and grants on `org.events` come in later pull requests: until then the token file's people are the org admins and its agents and ingest workers belong to the first team. Tracked in [#185](https://github.com/GetFelix/KBL/issues/185).

Today one KBL instance serves one team: one tracker, one `work.events`, one
Felix namespace and one token file. A second team needs a second deployment.
This section describes how one instance serves several teams, each isolated
from the others, and how a running instance gains a team without a restart.

The rules that hold today still hold:

- The tracker is the only writer of each team's `work.events`, ready streams
  and item keys.
- A check that refuses a record runs in the tracker before the append, so an
  old log folds exactly as it did.
- KBL runs no models.
- Felix grants are the enforcement. A token for one team gets nothing from
  another team's resources, even with a raw client.

### The data model

| Thing | What it is | Where it lives |
|---|---|---|
| Org | The whole instance. One org is one Felix tenant | `org.events` in the org namespace, `kbl` |
| Team | A group of people and agents with its own queue. One team is one Felix namespace, and its name is the namespace's name | A `team_created` record on `org.events`. The team's work is in its namespace's `work.events` |
| Person | Someone who signs in. A person is org-wide and can belong to several teams | `person_added` on `org.events`, with the hash of their tracker token and, optionally, their OIDC subject |
| Member | A person in one team, with one role there | `role_granted` on `org.events`, naming the team, the person, the role and who granted it |
| Role | What a member may do in the team: `viewer`, `reviewer`, `triager` or `admin` ([#120](https://github.com/GetFelix/KBL/issues/120)). `org_admin` is the one role outside any team | On `role_granted` |
| Agent | A name and a token, bound to exactly one team ([#181](https://github.com/GetFelix/KBL/issues/181)) | `agent_added` on `org.events`, with the token's hash and the person who added it |
| Source | An upstream repo or project (`github:GetFelix/felix`) owned by exactly one team | `source_added` on `org.events` |
| Ingest worker | A token bound to one team, for that team's sources | `ingest_added` on `org.events` |
| Grant | Lets one team's agents file prerequisites into another team's Triage | `grant_added` on `org.events`, written by an admin of the receiving team |

Names follow the key rule (1 to 64 characters of `[A-Za-z0-9_-]`). A team's
name must also be a valid Felix namespace name, and `kbl` is reserved for the
org. Agent names are unique within a team, so `w1` can exist in two teams.
People names are unique in the org.

`org.events` is a log like `work.events`: the tracker folds it on start and is
its only writer, with a lease of its own. The fold gives the tracker its
tables (which teams exist, who is in which, which token is whose) so nothing
about membership is kept anywhere else. Tokens are never on the log, only the
SHA-256 of each one. The tracker compares the hash of the presented token in
constant time, as it compares tokens today. A token is 32 random bytes, so its
hash cannot be reversed. `org.events` has its own format, starting at 1, under
the same rules as `work.events` ([upgrades.md](upgrades.md)).

### Teams in Felix

One org is one tenant. Teams are namespaces in it, and the org's own
resources are in the namespace `kbl`.

```mermaid
flowchart TB
    subgraph T["Tenant (the org)"]
        subgraph K["namespace kbl"]
            OE["org.events"]
            OT["org.tracker<br/>org writer lease"]
        end
        subgraph A["namespace payments (a team)"]
            AE["work.events"]
            AR["work.ready.p0-p3<br/>work.review.r0-r3"]
            AT["work.tracker<br/>lease, ingest state"]
            AX["agents.*.run-id<br/>a run's resources"]
        end
        subgraph B["namespace web (a team)"]
            BE["work.events"]
            BR["work.ready.*, work.review.*"]
            BT["work.tracker"]
        end
    end
    TR["kbl-tracker<br/>one process"] -->|"token narrowed to kbl"| K
    TR -->|"token narrowed to payments"| A
    TR -->|"token narrowed to web"| B
```

**Why one tenant per org, not per team.** A Felix token belongs to one tenant
and RBAC is stored per tenant, so nothing can be granted across tenants
(security.md, "Multi-tenancy and isolation", in Felix). A person in two teams
would need two sign-ins, and a cross-team prerequisite would have no grant to
rest on. Namespaces are what Felix offers as the isolation unit for teams
inside a tenant, and grants already stop at a namespace. Several orgs in one
instance is out of scope: run one instance per org.

**Resource names do not change.** Each team's namespace holds the same names a
single-team instance uses today: `work.events`, `work.ready.p0` to `p3`,
`work.review.r0` to `r3` with the `agents` group on each, and the
`work.tracker` cache. A run's resources (`agents.log.<run>` and the rest) are
created in the namespace of the team that starts the run. The org namespace
holds `org.events` and the `org.tracker` cache.

**Principals and RBAC.** Every RBAC object names its namespace
(`stream:{tenant}/{namespace}/{stream}`), so a rule written for one team
covers nothing in another. `kbl-tool team create` writes these rules:

| Felix role | Policies | Assigned to |
|---|---|---|
| `kbl-tracker` | `ns.manage` on `namespace:{t}/{team}`, one policy per team, and on `namespace:{t}/kbl` | The tracker's principal |
| `kbl-{team}-viewer` | `stream.subscribe` on `stream:{t}/{team}/work.events` | Each member with an OIDC subject, by `member add` |
| Per run, as today | The run grants in [Grants](#grants), naming resources in the team's namespace | A run's agent principals and viewers, by `run create --team` |
| `kbl-org-admin` | `tenant.manage` on `tenant:{t}` | The people who run `kbl-tool` for the org |

The tracker signs in once and exchanges its ID token for one Felix token per
namespace, narrowed with `resources: ["namespace:{t}/{team}"]` (token
exchange, security.md in Felix). Each team's fold publishes only through its
own team's token. A bug that routed team A's record to team B's connection
would be refused by the broker, not just by the tracker's code.

No team principal holds anything on `kbl`. Only the tracker and org admins
read `org.events`.

### One tracker, many teams

One `kbl-tracker` process serves every team. Inside it each team is what a
whole tracker is today: its own fold, ready lanes, delivery table, sweep,
writer lease and Felix connections. The org has one more fold, of
`org.events`.

- **A lease per team, plus one for the org.** Each team's lease is the
  `writer` key in its own `work.tracker` cache, the key a single-team tracker
  holds today, so migration keeps it. The org lease is the `writer` key in
  `org.tracker`. A lease per team keeps a stray second tracker out of every
  team, and lets a later change spread teams across trackers
  ([#124](https://github.com/GetFelix/KBL/issues/124)).
- **Ingest per team.** An ingest token is bound to one team and writes only to
  that team's log, on roots from sources that team owns. Its saved state stays
  in the team's `work.tracker` cache. One `kbl-ingest` per team is the simple
  layout. One process can also serve several teams with one config block and
  token each.
- **Failure stays in its team.** A team whose lease cannot be renewed, whose
  log has a record of a newer format, or whose Felix calls fail answers 503 on
  its own routes and is retried in the background. The other teams keep
  serving. A team that loses its lease drops its fold and rebuilds it from the
  log when it takes the lease again, as a restarted tracker does today. If the
  org lease is lost, teams keep serving with the membership they had, and org
  changes (new teams, members, agents, revocations) answer 503 until it is back.
  The process exits only when it cannot hold the org lease for 60 seconds,
  because then revocations would stop taking effect.
- **What is still shared.** One process means one crash or one out-of-memory
  takes every team down, and teams share its CPU. Each team gets a cap on
  calls in flight so one busy team cannot starve the rest. Running several
  trackers is #124.

### Adding a team with no restart

All three commands call the tracker's org routes with the caller's tracker
token. The tracker checks the caller's role, appends to `org.events`, folds
the record, and acts on it at once. `team create` also writes Felix resources
and grants, using the caller's Felix sign-in (`KBL_ID_TOKEN`).

| Command | Who may run it | What it does |
|---|---|---|
| `kbl-tool team create <team>` | Org admin | Creates the namespace and the viewer and runs roles in Felix, adds the namespace to the `kbl-tracker` role, then `POST /v1/org/teams`. The tracker writes `team_created`, creates the team's streams and group as it does on start today, takes the team's lease, and serves it |
| `kbl-tool member add <person> --team <team> --role <role> [--subject <oidc sub>]` | The team's admins, and org admins | `POST /v1/teams/{team}/members`. A new person gets `person_added` and a tracker token, printed once. Then `role_granted`. With `--subject`, it also assigns the person the team's viewer role in Felix for the dashboard |
| `kbl-tool agent add <name> --team <team>` | The team's triagers and admins | `POST /v1/teams/{team}/agents`. Writes `agent_added` and prints the token once. The token works on the next call |
| `kbl-tool agent revoke <name> --team <team>` | The person who added it, and team admins | `agent_revoked`. Every hold of that agent in the team is released with the reason `revoked`, as a lapsed claim is |
| `kbl-tool member remove <person> --team <team>` | Team admins, org admins | `role_revoked`, then removes the Felix assignment, which ends the person's open dashboard streams (revocation, security.md in Felix) |
| `kbl-tool source add <source> --team <team>` | Team admins | `source_added`. Refused when another team owns the source |
| `kbl-tool ingest add <name> --team <team>` | Team admins | `ingest_added`, prints the token once |

Every step can be run again. The control plane answers that an existing
namespace or resource already exists, and the tracker answers `created: false`
for a team, member or source it already has. A `team create` that stopped
part way is finished by running it again.

Membership changes take effect on the next request, because the tracker's
token table is the fold of `org.events`. There is no reload step.

### Roles inside a team

#120's four roles apply per team. A member has one role in a team; a person
can be a viewer in one team and an admin in another. A role can also be
narrowed to some of the team's repos, which is #120's "per repo" case. The
check runs in the tracker on each person route, before the append, so a
refused call writes nothing and the fold of `work.events` does not change.
`org.events` records who granted each role.

| Role | Adds to the role before it |
|---|---|
| `viewer` | The dashboard, read only |
| `reviewer` | Answer questions (`/v1/work/questions/answer`) |
| `triager` | Accept, edit and reject proposals; change tiers; close and unlink; add agents |
| `admin` | Budgets, rules, raw records (`/v1/work/records`); members, sources, ingest tokens and grants for the team |
| `org_admin` | Create teams, and act as admin in every team. Raise a log's format |

### Cross-team prerequisites and grants

Today's `grants` let an agent file a prerequisite into another repo. With
teams, a repo can belong to another team, and so can the Triage the
prerequisite lands in. Filing into another team's queue needs a grant from that
team: `grant_added {to_team, from_team, agents?, repos?}`, written by an admin
of the receiving team. Without one, the agent gets the same `unknown_item`
answer it gets for a repo it cannot see.

A prerequisite in one team is one record on one log. Across teams it is two
logs, so the tracker writes two records in a fixed order:

1. On the receiving team's log, the proposal, with `from: {team, item}` and an
   id derived from the waiting item and the agent's post key. A retry finds the
   proposal that is already there and writes nothing.
2. On the waiting team's log, the `prerequisite`, naming the foreign item
   `{team, item}`. The held item waits on it and the claim ends as `blocked`,
   as it does today.

If the tracker stops between the two, the receiving team has a proposal
nobody waits on, which a person sees in Triage like any other, and the agent's
call failed, so its retry links to that same proposal. When the foreign item
closes, the tracker appends `prerequisite_settled {item, foreign, outcome}` to
the waiting team's log. On start it checks every unsettled foreign link
against the other team's fold and writes what is missing. The waiting team's
fold never reads another log, so each team's log still replays on its own.

The foreign link and `prerequisite_settled` are new on `work.events`, so they
need a new format. A log with no cross-team prerequisite stays at the format
it had.

A cross-team proposal counts toward the filing agent's pending-proposal caps,
and the receiving team can set its own cap per granting team.

`work_link` stays inside one tree, so it stays inside one team.

### The dashboard

The dashboard asks the tracker `GET /v1/me` for the teams the person belongs
to and their role in each, and shows a team switcher. The switcher lists only
those teams. The team is in the URL, so a link opens the same team.

What the page can read is decided by Felix, not by the switcher: the person's
Felix token carries the viewer role of their own teams only, so asking the
gateway for another team's `work.events` is refused.

felix-gateway serves one namespace per process (`GATEWAY_NAMESPACE`), so until
it can take the namespace from the scope, each team needs a gateway of its
own, and `team create` prints the settings for it. Starting that gateway is
the one step of adding a team that is not live. This is gap 10 in [Risks](#risks-and-what-this-surfaces-in-felix).

### Moving a single-team instance to teams

The existing namespace becomes the first team. Its `work.events` is not
touched, rewritten or copied.

1. Upgrade the tracker to a build with teams. Without an org namespace it runs
   exactly as before: the namespace in `KBL_NAMESPACE` is its only team, and
   the token file is read as today.
2. Run `kbl-tool org init` as an org admin. It creates the `kbl` namespace,
   `org.events` and `org.tracker`, and gives the tracker's role `ns.manage` on
   `kbl` and on the existing namespace.
3. Restart the tracker with `KBL_ORG_NAMESPACE=kbl`. This is the only restart.
   It finds `org.events` empty and appends `team_created` for the namespace in
   `KBL_NAMESPACE`, marked `adopted`. It keeps that team's existing lease key,
   streams and group.
4. The token file keeps working. Its agents and ingest workers belong to the
   first team, its `grants` are grants inside that team, and its people are
   org admins, which is what every person can do today. Moving them onto
   `org.events` with `member add` and `agent add`, then removing them from the
   file, is up to the operator.

Going back to the previous build works until a second team or a cross-team
prerequisite exists: the old build reads the same `work.events` and ignores
`org.events`.

### Out of scope

- Several orgs in one instance. Each org runs its own instance and tenant.
- Moving an item or a tree from one team to another. A person closes it in one
  team and files it in the other.
- Signing people in to the tracker with OIDC. Person tokens stay tracker
  tokens ([#121](https://github.com/GetFelix/KBL/issues/121) covers their
  lifetime).
- Quotas per team on Felix storage or subscriptions, which Felix does not
  have yet.
- Running several trackers at once (#124). The per-team leases make it
  possible later; this design does not do it.
- Metrics across teams. Each team's metrics come from its own log.

### Pull requests

In order. S is under 300 changed lines, M up to 800, L more.

| # | Change | Size |
|---|---|---|
| 1 | `core`: the `org.events` records and fold, token hashes, the team and name rules, golden logs | M |
| 2 | Tracker: one fold, lease and connection set per team behind a team table; the team comes from the token; `/v1/teams/{team}/work/...` beside today's paths; `GET /v1/me`; per-team failure isolation and in-flight cap. One team only, adopted from `KBL_NAMESPACE` | L |
| 3 | `kbl-tool org init` and `team create`, the org routes, Felix provisioning and roles, `dev/check-teams.mjs` with two teams and a raw client refused across them | M |
| 4 | Members and roles in a team (#120): `member add`, `remove`, `role_granted`, the role check on every person route | L |
| 5 | Agents per team (#181): `agent add`, `list`, `revoke --team`; revoke releases holds | M |
| 6 | Ingest per team: team-bound ingest tokens, `source add`, one owner per source, team blocks in the ingest config | M |
| 7 | Cross-team prerequisites and grants: a new format, the foreign link, `prerequisite_settled`, the restart check | L |
| 8 | Dashboard: `/v1/me`, the team switcher, team in the URL; a gateway per team until gap 10 is closed | M |
| 9 | Runs per team: `run create --team` and the runs role; the grants test with a raw client per team ([#32](https://github.com/GetFelix/KBL/issues/32)) | S |

Each PR updates [threat-model.md](threat-model.md) for what it adds.

## Build order

The work tracker in [work-tracker.md](work-tracker.md) comes first: it is the
part that does real work. Its first source is GetFelix/felix itself, issues
labelled `agent-ok`, and an agent's result is a pull request that people review
and merge. The demonstrations are numbered in
[What it has to demonstrate](#what-it-has-to-demonstrate).

| M | Milestone | Proves |
|---|---|---|
| 0 | The dashboard and the decision replay, locked: style frames drawn from hand-written fixtures | They read well before Felix is involved |
| 1 | Tasks through Felix: `core`, the dev stack, `kbl-tool`, `kbl-mcp` task tools, scripted planner and workers | Demonstrations 1 and 2 |
| 2 | The tracker: GitHub ingest, items and dependencies, ready tiers, `work_next` and the rest of the work tools, triage and augmentation as claimable work | Real issues become claimable work, once each, and people and agents never overwrite each other (demonstration 3) |
| 3 | Real work: Claude Code claims `agent-ok` issues through `kbl-mcp` and opens pull requests | Agents produce reviewed, merged fixes |
| 4 | The dashboard, live, and more sources (My queue, Board, Metrics) with the review limit, and its results: arrival and completion, time waiting on a human, merge rate, cost and human minutes per fix | Anyone can see the queue and what it delivered |
| 5 | Shared state (closed, superseded by KBL: the swarm demo was removed) | Nothing |
| 6 | The reference worker (`kbl-run`): a model API, artifacts, spend and rate limits | Agent-created work stays inside its budget (demonstration 4) |
| 7 | Decision replay, live: the fold, the dashboard on felix-gateway, scrub and follow | The log is a complete record that replays, and a fix's decisions read back with no hindsight (demonstrations 5 and 6) |
| 8 | A hostile agent stays bounded: grants and an injected agent held to its budget | A prompt-injected agent cannot escape its budget or grants (demonstration 8) |
| 9 | Any host, at scale: the Agent SDK as a host, 50 agents, broker restart and failover | Any MCP host can take work (demonstration 7), and it holds under loss |
| 10 | Self-hosting: `kbl-mcp` releases, compose, a quick start | Anyone can run it |

**Why it was done this way.** [decision-replay.md](decision-replay.md) records an
agent's decisions with their evidence and reads them back as a story for
reviewers: recording in M3, the story view in M4, full replay and questions in M7.

**M0 came first on purpose**, for the reason the other showcases give: a
view nobody can read proves nothing, so the look was fixed in style frames
before anything was built behind it.

## Risks, and what this surfaces in Felix

- **Cost of tests.** Agents on a real model cost money. The scripted planner and workers run the same flows for CI and for anyone without a key.
- **Visibility vs. long calls.** A claim lives 90 seconds without an extension. A `kbl-mcp` that stalls longer than that (a paused laptop) loses its task. That is the intended behaviour, and the duplicate check handles the result.
- **Scope.** No feature that does not serve one of the eight demonstrations or the dashboard.

**Felix gaps this design runs into**

Checked against Felix at `97ef3827` (paths in Felix unless marked) and felix-gateway 0.6.0 (`84dfa11`).

1. **Max attempts is broker-wide.** `FELIX_GROUP_MAX_ATTEMPTS` (default 5, `services/felix-broker-service/src/config/defaults.rs:48`) applies to every group; `GroupPoll` has no attempts setting (`crates/protocol/felix-wire/src/client/message.rs:898-936`). Expensive tasks want two. Workers count attempts themselves until a group can say so.
2. **No agent identities without an outside issuer.** A principal comes from an upstream OIDC token (`services/felix-controlplane-service/src/auth/exchange.rs:1-7`), and delegation keeps the delegating subject (`auth/delegate.rs:8-11`). A run's agents need their own subjects, so `kbl-tool` has to run an issuer. A way to mint narrower tokens for named sub-principals would remove it.
3. **No request/reply.** Not needed: asking is question work in the tracker ([decision-replay.md](decision-replay.md)), which needs no reply routing.
4. **No bounded counter add.** `CounterAdd` is an unconditional signed delta (`message.rs:814-820`), and a retried add counts twice (`docs/projections.md:164-170`). A budget needs reserve, check and refund, and a refund that is retried undercounts. An add that applies only below a limit, with a dedupe id, would make it one safe call. Counters with a TTL would retire rate-limit windows.
5. **felix-gateway's client cannot read a range.** The gateway relays `read` (`crates/felix-gateway/src/protocol.rs:122-137`), but `felix-gateway-client` has no method for it and ignores `records` (`packages/gateway-client/src/index.ts:195`). The dashboard needs it to scrub.
6. **No time lookup through felix-gateway.** Felix has `offset_for_time` (`crates/sdk/felix-client/src/client/subscribe.rs:248`); the gateway's `ClientMessage` does not (`crates/felix-gateway/src/protocol.rs:74-158`). "Go to 14:05" has to bisect by reads.
7. **felix-gateway drops the recorded publisher.** Felix records who published each event (`what-felix-is-for.md:164`), but the gateway's `event` and `records` carry only offset and payload (`crates/felix-gateway/src/protocol.rs:195-203, 230-238`). `stamp_sender` covers writes made through the gateway, not the agents' writes over QUIC (`crates/felix-gateway/src/scope.rs:64`). Without it the dashboard cannot show verified authorship.
8. **The Node and Python bindings lag the Rust client.** No `cache_put_if`, `cache_delete_if`, `group_extend`, per-poll visibility, `read`, `offset_for_time` or publisher identity (`crates/sdk/felix-typescript/src/client.rs:282-616`, `group_poll` at `:491-504`). `kbl-mcp` is Rust because of it. An agent written against the bindings directly could not take a lease.

9. **felix-gateway drops the broker's append time.** Felix (at `d40db2e4`) stamps every record with its append time by the shard leader's clock: range-read records always carry it (`crates/protocol/felix-wire/src/client/message/fields.rs:192-193`), and subscribed events do when the client asks (`ClientConfig::timestamps`, `crates/sdk/felix-client/src/config.rs:122-126`; `Event::timestamp_micros`, `crates/sdk/felix-client/src/subscribe.rs:235-239`). felix-gateway's `event` and `records` carry only offset and payload (`crates/felix-gateway/src/protocol.rs:195-206, 325-331` in felix-gateway). Records written by agents on different machines then have only their writers' clocks, which skew, so a claim can be drawn before the post it claims. The dashboard orders by offset and clamps writer times to rise with it until the gateway passes the broker's time on.

10. **felix-gateway serves one namespace.** `GATEWAY_NAMESPACE` is set once per gateway process (`docs/configuration.md` in felix-gateway 0.8.0), and a scope's `{scope}` value fills in resource names, not the namespace. A dashboard for several [teams](#teams) needs one gateway per team until a scope can name the namespace.

**Decisions on the earlier open questions**

- **`kbl-mcp` speaks MCP over HTTP as well as stdio, but never holds tokens for callers.** stdio comes first and stays the default. HTTP is added for hosts that cannot start a local process. Each HTTP caller sends its own bearer token, and `kbl-mcp` uses that token for that caller's requests and nothing else, so grants still decide what each agent can write. A shared server that kept tokens on behalf of its callers would turn one compromised server into every agent's credentials.
- **A claim lasts 90 seconds, extended every 30 seconds, whatever the task.** This is the usual visibility timeout plus heartbeat, as in SQS or a lease with renewal. It does not try to predict model latency: a call that takes five minutes keeps its task because `kbl-mcp` keeps extending, and a worker that dies loses it within 90 seconds. Tying the timeout to the expected latency would only make recovery slower for long tasks, and the task's own deadline already bounds how long anything can run.
- **Runs share a namespace, with the run id in each resource name.** A namespace is a team's or a project's, which is how namespaces in Felix and most multi-tenant systems are meant to be used. A run is cheap and short-lived, and its resources (`agents.tasks.<run>` and the rest) are created when it starts and deleted when it is no longer needed. Grants name the run's resources, so one run's agents cannot reach another's. A namespace per run would make starting a run an operator step and spread one team's runs across many namespaces.

