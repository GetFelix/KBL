# Flow metrics

The dashboard's Metrics dialog shows how work flows, what it costs and how much a
person had to step in. Every figure is derived from `work.events` by the same
fold the tracker runs, in the page (`metrics_json` in the `wasm` crate). No record
or format changes, and nothing is estimated or asked of a model. The Prometheus
endpoints are a different thing: see [metrics.md](metrics.md).

These are team numbers. None is per person or per agent, and none will be: a ranked
person turns a measure into a target.

## What counts

- **Accepted records only.** A record the fold refused changed nothing, so it counts
  nowhere. A `refused` record the tracker logs is not work either.
- **Window.** The last 7, 30 or 90 days. A stage stay belongs to the window where it
  ended. A done item belongs to the window where it last became done.
  The window ends now, or at the newest record when the browser clock is behind it; the
  dialog then says the clock is behind.
- **Done.** Merged, or finished work that needs no merge. An item reopened since is
  not done, so a redo or reopen counts the item once, when it is done again.
- **Done items.** The per-item figures below average over every item done in the
  window, of any kind (fix, review, triage). Items still open are never in them.

## Time in each stage

p50 and p85 of how long items stayed in Triage, Needs you, Blocked, Ready, Held by an
agent, In review and Proposed before leaving it, by nearest rank. The stage of an
item is the fold's own (`State::stage`), re-read for the whole tree after each record
that can move it. A stay that has not ended is left out. Time is the records' own
timestamps, so a tracker clock that stepped back gives a zero stay, never a negative
or a huge one. The fold keeps the last 50,000 finished stays; a window that reaches
back past the oldest dropped one says so.

## Throughput

Items done per day and per week over the window. Weeks are counted back from now, so
the oldest may be a part week.

## Rework

Over the items done in the window, the count and the average per done item of:

- **Redos**: forks of an attempt (`redos` on the item).
- **Reopened**: accepted `reopened` records, whenever they happened, on those items.
- **Failed attempts**: accepted `released` records that failed (or gave no reason)
  and `gave_up` records. A release that waits on a person or on something outside
  KBL is not a failure.

## Cost

What the done items cost, from the recorded spend: the total, the average per done
item, and per model where usage was recorded by model. Failed attempts are part of
the item's cost. Cost reported with no model is in the total only. A model with no
price yet is marked: its cost is a floor, as in Spend.

## Supervision

For the items done in the window:

- **Items a person touched**: done items with at least one accepted record written
  by a person, and their share.
- **Records a person wrote** per done item: how often a person stepped in.
- **Waiting on a person**: per done item, the time spent in Needs you or In review
  over its life, p50 and p85.

With nothing done in the window the per-item figures show a dash, not zero.
