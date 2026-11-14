Format 4: a redo of an item whose pull request has a failed check is handed
out once the item has a note written since the failure, and a released or
lapsed claim on it can be retried. If the checks fail again, a new note frees
it; no new redo is needed. Before, the item stayed in Needs you while the check was red, so the redo
waited for green and its claim was refused with `not_ready`. Format 4 adds no
record; it changes how the fold reads an existing log, so it applies only after
a person raises each team's log to format 4 (`format_raised`, see
docs/upgrades.md), once every tracker and dashboard page reading that log is
this build or newer.
