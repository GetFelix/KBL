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
2. **Ingest and `felix-mcp`.** Any order, any pace. They only call the
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
   Everything else works without it. Today there is only format 1, so
   there is nothing to raise yet and the tracker refuses the record.

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
is 1.

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

Ingest and `felix-mcp` call the tracker over REST. The rule is
the same one Felix uses on its wire: a new request field is optional and its
absence means the old behaviour, and a caller ignores reply fields it does not
know. The tracker is upgraded first, so a caller never asks for a route the
tracker does not have yet.

## How CI holds this

- **Old logs.** `core/tests/format.rs` reads logs written before formats
  existed and checks that they are format 1, are written back without `v`,
  and fold to the same state with or without `"v":1`. The stored states in `wasm/golden`
  did not change when formats were added.
- **The replay corpus** ([#117](https://github.com/GetFelix/KBL/issues/117))
  will keep a log from every release with its folded state. Each record in a
  stored log carries the format its release wrote, so the corpus pins every
  format ever shipped: CI folds each log with the current core and compares.
  A log that holds a `format_raised` pins the raise as well.
- **Old and new builds on one log.** `dev/check-upgrade.mjs` runs the previous
  tracker build and the new one in turn over the same `work.events`, forward,
  back and forward again, with agents on the previous `felix-mcp`. Claims
  survive each switch and each item is claimed and done once. Both builds
  write format 1 only, so this run does not show a reader stopping at a newer
  format; the unit tests of `core`, the tracker and the page do. "Previous" is
  the commit a change is based on until KBL has releases; then it is the
  last release.

## Not covered yet

Run logs (`agents.log.<run>`) and task records have no format. Readers ignore
unknown fields and skip unknown kinds there. Tracked in
[#166](https://github.com/GetFelix/KBL/issues/166).

The ready streams are derived from `work.events`: a tracker publishes every
ready item again when it starts, and drops a ready record it cannot read.
