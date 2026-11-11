# A work tracker for coding agents

A proposal, not yet on the build order. It applies the swarm in
[design.md](design.md) to a job that never ends: bugs arrive from an upstream
tracker, an agent triages them, and coding agents ask for the next piece of
work, fix it and hand it back.

## The problem

Bugs arrive in GitHub Issues or Azure DevOps. Agents fix them, but three
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

A service, `agents-tracker`, with a REST API for people and tools and the same
operations as MCP tools in `felix-mcp` for agents:

| Tool | Does |
|---|---|
| `work_next` | Claims the highest-priority ready item the agent can take |
| `work_extend` | Keeps the claim while the agent works (done by `felix-mcp`, as for tasks) |
| `work_context` | Reads or adds context on an item |
| `work_link` | Adds a dependency, or splits an item into child items |
| `work_progress` | Records a step: a plan, a branch, a PR |
| `work_complete` / `work_release` | Finishes the item or hands it back |

An ingest worker pulls new and changed items from GitHub or ADO. A triage
agent then dedupes them, sets priority, links dependencies, splits large items
and writes its reading of each bug as context. People edit any of that through
REST or the viewer.

## The Felix mapping

It reuses the pieces the swarm already has. No database.

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
  Agents claim from that stream through a consumer group, with the 90 second
  visibility and 30 second extension that tasks use. A dead agent's item goes
  back to the queue.
- **Priority is a stream per tier.** A group hands records out in offset order,
  so `work_next` polls `work.ready.p0` first, then `p1`, and so on.
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

## The intake view

The swarm view shows one run. The intake view shows everything before and
around runs: what arrived, what is waiting and why, who holds what, and where
work is piling up. It is the page someone opens first in the morning.

```text
+---------+-----------+-----------+-------------+-----------+--------+------+
| Sources | Arrived   | Needs you | Blocked     | Ready     | Held   | Done |
|         | (12)      | (3)       | (7)         | p0 2      | (5)    | (41) |
| GH  +9  | #812 2m   | #790 asks | #801 <- 798 | p1 6      | #798 w2| 24h  |
| ADO +3  | #811 5m   |   repro?  | #799 <- 798 | p2 11     | #805 w5|      |
|         | AB#4410   | #774 2x   |             | p3 4      | ...    |      |
|         |           |   failed  |             |           |        |      |
+---------+-----------+-----------+-------------+-----------+--------+------+
| in 14/day   out 11/day   oldest ready 3d (#702)   timeline ...  live    |
+--------------------------------------------------------------------------+
```

- **Columns are stages.** Arrived (not yet triaged), Needs you, Blocked, Ready
  by tier, Held by an agent, Done. An item sits in exactly one, and moves
  right as `work.events` records it. Each column shows its count and the age
  of its oldest item, since a stall shows as age long before it shows as a
  count.
- **Needs you is its own column.** Items where triage asked a question, an
  agent gave up, or attempts ran out. This is the human queue, and the
  reason to open the page.
- **Blocked shows what it waits on.** Selecting an item draws its dependency
  chain across the columns, so a whole branch stuck behind one held item is
  visible at once.
- **Sources show arrival.** Per upstream tracker, items in the last hour and
  day. The footer compares arrival and completion rates: a queue that grows
  every day needs more agents or fewer bugs, and the view should say which.
- **Held links to the run.** A held item names its agent, and opening it
  opens that run in the swarm view at the moment it claimed the item.
- **It scrubs like a run.** The view is a fold of `work.events`, so the same
  timeline shows the queue as it was on any day.

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
  means the tracker, like `felix-mcp`, is Rust. Gaps 5 to 7 limit the viewer
  as they do for runs.

## Order

After M4, when replay works: ingest from GitHub, `work_next` and the ready
streams, then triage, dependencies, and the intake view. ADO ingest after
GitHub.
