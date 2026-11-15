Format 3: triage posts prerequisites. An agent holding an item's triage work
can call `work_post` with `work: "prerequisite"`, and the item under triage
waits on that work in another repo before any build claim is handed out. The
triage claim goes on and nothing is charged by the call. The tracker refuses
it with `format` until a person raises each team's log to format 3
(`format_raised`, see docs/upgrades.md), after every tracker and dashboard
page reading that log is this build or newer.
