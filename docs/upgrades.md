# Upgrading KBL

How to move a running KBL (Kanban By Log, pronounced Kibble) to a newer build without losing work, how to go
back, and the record rules that make both safe.

## Upgrade

Upgrade in this order. No work is lost at any step.

1. **Tracker.** Stop the old tracker and start the new one. The new one takes
   the writer lease when the old one's runs out (15 seconds at most) and
   rebuilds its state from `work.events`. Calls to the tracker fail during
   that handover. A claim stands for 90 seconds without an extension, so
   agents keep the work they hold. Running more than one tracker, which
   removes the pause, is
   [#124](https://github.com/GetFelix/KBL/issues/124).
2. **Ingest and `kbl-mcp`.** Any order, any pace. They only call the
   tracker's routes, and a newer tracker serves every older caller.
3. **Dashboard.** Publish the new page. Open pages keep working until they
   reload.
4. **Raise the format**, if the release notes say the release has a new one.
   Do this last, when nothing older is left running:

   ```bash
   curl -X POST "$TRACKER/v1/work/records" \
     -H "authorization: Bearer $PERSON_TOKEN" -H 'content-type: application/json' \
     -d '{"record": {"kind": "format_raised", "to": 2}}'
   ```

   Features that need the new format are refused with `format` until then.
   Everything else works without it. With [teams](design.md#teams) each team
   has its own log, so raise each one, at
   `$TRACKER/v1/teams/<team>/work/records`. The formats so far are under
   [Formats](#formats).

You may skip releases: any older build can be upgraded to any newer one.

**Before the first raise, check for builds from before formats.** A build
older than the one that added formats does not know `v`. It ignores the
field, reads a newer record as format 1 and carries on with a wrong state. It
does not stop. The tracker cannot see which builds are reading the log, so
this is on you: before raising the format for the first time, make sure no
such tracker is running or set to restart, and that every open dashboard page
has been reloaded since the upgrade. Every build from this one on stops by
itself.

## Go back

Until step 4 you can go back to the old build at any time, in the reverse
order. Nothing the new build wrote is beyond the old one.

After the format is raised you cannot go below a build that reads it. An older
tracker refuses to start on that log and says which offset stopped it, without
taking the writer lease, so it does not keep a newer tracker out. An older
dashboard page stops and asks for a reload. Neither one skips the record and
carries on. This holds for builds that have formats; a build from before
them misreads the log instead, as the warning above says.

The format never goes down. To undo a raise, restore a backup taken before it.

## What a format is

Every record on `work.events` has a format, a whole number in its `v` field.
A record with no `v` is format 1. That is every record written before formats
existed, and format 1 is frozen: its records are written without `v`, byte for
byte as before.

Each build has one number, `FORMAT` in `core/src/work.rs`: the highest format
it reads and writes. It reads every format from 1 up to that. Today `FORMAT`
is 2.

### Formats

| Format | Adds | What waits for the raise |
|---|---|---|
| 1 | Every record written before formats existed | Nothing |
| 2 | `returned_to_scope`: an issue that matches its intake rule again leaves Needs you | Until the raise, an item whose issue comes back into scope stays in Needs you, as it did before. Ingest writes the record at its next reconcile after the raise, or sooner if the issue changes |

`org.events` ([teams](design.md#teams)) has its own number, `ORG_FORMAT` in `core/src/org.rs`, also 1. It follows the same rules and has no `format_raised` yet, since nothing needs format 2.

The log has a format too. It starts at 1 and a `format_raised` record moves it
up. Only a person can write one.

## Rules for readers

A reader is anything that folds `work.events`: the tracker and the dashboard
page.

| The record | What the reader does |
|---|---|
| Format the build reads, known kind | Folds it |
| Format the build reads, with a field the build does not know | Folds it and ignores the field |
| Format the build reads, with a kind the build does not know | Skips it. Only logs from before formats can hold one: the tracker now refuses to log a kind it does not know |
| Not JSON, or `v` is not a whole number from 1 up | Skips it, with a line in the tracker's output |
| Format above what the build reads | **Stops.** The tracker does not serve. The page asks for a reload |

Stopping is the point of the last row. A reader that skipped a record it could
not read would fold a different state from the tracker that wrote it, and then
hand out work or show stages from that wrong state.

## Rules for writers

The tracker stamps each record with the lowest format whose readers fold it
the way this build does (`WorkRecord::format`). A record that uses nothing new
is still written as format 1, even by a much later build.

The fold refuses a record stamped above the log's format. So a new tracker
writes nothing an older one cannot read until a person raises the format.

A `format_raised` record is stamped with the format it raises to. A reader
that cannot read what follows therefore stops at the raise itself.

## Rules for changing the fold

Ask one question about a change to a record or to the fold: would a build
that does not have the change fold any log to a different state?

- **No.** Keep the format. The usual case is a new field the fold does not
  read, such as text that is only shown to people.
- **Yes.** Add 1 to `FORMAT`, and make `WorkRecord::format` return the new
  number for the new kind or for a record that uses the new field. A new kind
  is always a yes, and so is a new field the fold reads. Such a field is
  optional, and a record without it folds exactly as before.

The fold of an old log must never change. A build folds a format 1 record the
way the first build did, forever. The one exception is an item's reported
cost total, which saturates at the largest number where the first build
overflowed.

A new kind cannot be missed: `WorkRecord::format` names every kind, so the
build fails until the new one has a format, and `core/tests/format.rs` holds
the frozen list of format 1 kinds.

A new field is different. Nothing in the build can tell that the fold now
reads a field it did not read before. So this is a review rule: a pull
request that makes the fold read a new field of an existing kind must raise
`FORMAT` and return the new number from `WorkRecord::format` when the field
is set. Reviewers check it.

## The tracker's routes

Ingest and `kbl-mcp` call the tracker over REST. The rule is
the same one Felix uses on its wire: a new request field is optional and its
absence means the old behaviour, and a caller ignores reply fields it does not
know. The tracker is upgraded first, so a caller never asks for a route the
tracker does not have yet.

## How CI holds this

- **Old logs.** `core/tests/format.rs` reads logs written before formats
  existed and checks that they are format 1, are written back without `v`,
  and fold to the same state with or without `"v":1`. The stored states in `wasm/golden`
  did not change when formats were added.
- **Each later format.** `core/tests/work_format_2.jsonl` raises a log to
  format 2 and uses it, including a format 2 record before the raise, which
  the fold refuses. Its state is pinned in `wasm/golden/format_2.state.json`
  and folded by the core, the WebAssembly build and the page's tests.
- **A run's own records.** `core/tests/run_format.rs` checks that every run
  log kind (the frozen `core/tests/run_log_format_1.jsonl`) and a task of the
  first format are written byte for byte as before formats, and that a newer one is refused, not skipped. `kbl-mcp`'s tests
  check that a worker hands back a task of a newer format without running or
  dead-lettering it, and that a result it cannot read still settles a
  redelivered task.
- **The replay corpus** ([#117](https://github.com/GetFelix/KBL/issues/117))
  will keep a log from every release with its folded state. Each record in a
  stored log carries the format its release wrote, so the corpus pins every
  format ever shipped: CI folds each log with the current core and compares.
  A log that holds a `format_raised` pins the raise as well.
- **Old and new builds on one log.** `dev/check-upgrade.mjs` runs the previous
  tracker build and the new one in turn over the same `work.events`, forward,
  back and forward again, with agents on the previous `kbl-mcp`. Claims
  survive each switch and each item is claimed and done once. The run never
  raises the format, so both builds write format 1 only and this run does not
  show a reader stopping at a newer format; the unit tests of `core`, the
  tracker and the page do. "Previous" is
  the commit a change is based on until KBL has releases; then it is the
  last release.

  A deliberate break during development, such as a rename of something both
  builds must agree on, can be declared with a line starting `Upgrade-Break:`
  in the message of any commit between the previous build and this one. The CI
  step then prints that line and skips this check; it never skips for a
  change that has not declared one.

## A run's own records

A run has records of its own beside `work.events`: its run log, its tasks
and the values in its caches. All of a run's programs are started together,
so two builds rarely meet inside one run. The rules below are for when they
do.

**Run log** (`agents.log.<run>`). Each record has a format in `v`, with the
same rules as `work.events`: no `v` means format 1, format 1 is frozen and
written without `v`, and `Record::format` in `core/src/record.rs` names a
format for every kind. A new field of an existing kind, or a new shape
inside a field, is a review rule as it is for the fold. `RUN_LOG_FORMAT` is 1 today. A reader that builds
anything from a run log reads it with `record::read` and stops at a record of
a newer format. Nothing decides from a run log today: `kbl-tool run log`
prints the records as they are, whatever their format.

**Tasks** (`agents.tasks.<run>`). A task has a format in `v` as well
(`TASK_FORMAT` in `core/src/task.rs`, 1 today, left out at 1). A worker runs
what a task says, so a worker that got a task of a newer format would run it
without the part it cannot read. Instead `task_claim` hands the task back,
logs a `refused` record and tells the agent to upgrade. The task is not
dead-lettered, so a newer worker can still take it. Handing it back uses up
one of the task's deliveries, and workers give up on a task after two. So
when a release raises the task format, upgrade every `kbl-mcp` of a run
before its planner posts with the new one. A `kbl-mcp` from before task
formats does not know `v` at all: it runs a newer task as if it were format
1 and hands nothing back, so none may be left in a run by then. A worker
that only reads format 1 also hands back a newer task that already has a
result, until Felix dead-letters it; nothing is lost, the log just shows the
refusals.

**Cache values** (results, leases, presence, the board) have no format, and
do not need one, because nothing is decided from what a value holds:

| Value | What is decided, and from what |
|---|---|
| A task's result | Whether the task is finished: from the key being there. The first write wins in Felix. A worker that cannot read the value still settles a redelivered copy; it only leaves out the `task_duplicate` record |
| A lease | Who holds it: from the cache version, which is the fencing token. The value is the holder's name, for people to read |
| Presence | Whether an agent is up: from the key and its TTL. The value is shown, not acted on |
| The board | Nothing. The text goes to a model |

Only a task's result has a reader today (`kbl-mcp`); no program reads a
lease, presence or board value back. A reader ignores a field it does not
know in these values. A change that
would make a reader decide from a value's content gives that value a format
first.

## Derived streams

The ready streams are derived from `work.events`: a tracker publishes every
ready item again when it starts, and drops a ready record it cannot read.
