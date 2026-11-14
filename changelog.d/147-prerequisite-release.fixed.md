Format 5: a prerequisite may name an existing item that is merged or completed
but not yet released when it waits for the release (`done_when: released`).
Before, it was refused as `closed`, so the agent had to post a duplicate new
prerequisite. A later prerequisite on the same link that asks only for the
merge no longer drops an earlier ask for the release; re-opening the
prerequisite does, since the waiter asks again in the new round. Format 5 adds no record;
it changes how the fold reads an existing log, so it applies only after a
person raises each team's log to format 5 (`format_raised`, see
docs/upgrades.md), once every tracker and dashboard page reading that log is
this build or newer.
