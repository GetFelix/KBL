# Fault sweep

The tracker's claim path and ingest each run under a seeded sweep of faults,
and check the log after every step. A seed is one deterministic run, so a
failure replays exactly.

| Sweep | Where | Faults | Invariants |
| --- | --- | --- | --- |
| Claim path | `tracker/src/tracker/tests/sweep.rs` | the log is created at the current format on each seed's first start, and a restart finds it and writes no second raise; the tracker dies after some of its writes and restarts from the log; a lost publish reply; a write a failover cuts before its reply (never an acknowledged record, which KBL requires Felix to keep: [work-tracker.md](work-tracker.md#what-felix-must-guarantee)), with another writer racing to take the freed offset; another writer takes the offset, with a claim or release of its own (a failover; on half the seeds a refusal burns the offset, as on a Raft shard); a replaced tracker that keeps serving until it is fenced; Felix hands a ready record out again, acked or not; a ready lane that fails to poll; leases that run out; agents claiming with a kind or tier filter, which takes the claim straight from the state; agents extending what they believe they hold; a supervisor that starts workers, stops them, has them finish, and is removed (every worker it has stops); a person reassigning an item an agent holds from a direct claim, then a restart; items that gain, change or lose needs, and agents whose declared capabilities change between claims | no acknowledged record is cut (a check on the faults themselves); a claimed item is never already held; a worker's token works exactly while its worker is live, across restarts; live workers stay inside the cap; a stopped or finished worker holds nothing after one sweep, and a record it writes after the stop is refused; a claim is handed to one agent, and an attempt to a second only after the first hold ended; an extension is granted only while the log shows the agent holding the item; work that needs something is handed only to a claimer whose declared capabilities cover every need, with none declared meaning only work that needs nothing; no attempt is done twice; a restart keeps a reassigned direct claim, polls no lane for it, and its holder can extend it; the log folds from nothing; a dismissed root stays dismissed and unheld; an imported item stays; an accepted item ends done, closed or waiting for a person |
| Ingest | `ingest/src/sync/tests/sweep.rs` | the process dies after some of its writes (appends and saves); a lost append reply; a stale save; the tracker unreachable; the saved state lost; GitHub failing; the same poll twice | an issue is imported once; a root a person dismissed is never reopened; once faults stop, every open issue is imported and admitted with its proposals, and a root is closed exactly when its issue is |

## Running it

```sh
cargo test -p kbl-tracker sweep                  # 40 seeds
cargo test -p kbl-ingest sweep
KBL_SWEEP_SEEDS=1000 cargo test -p kbl-ingest sweep   # seeds 0..1000
KBL_SWEEP_SEED=42 cargo test -p kbl-ingest sweep      # replay one seed
```

A failure prints `fault sweep failed on seed N` with the command that replays
it. The pull request job runs 400 seeds of each sweep. The nightly workflow
(`.github/workflows/sweep.yml`) runs 5000.

## Known failing seeds

Neither sweep has a known failing seed today. A bug the sweep finds on main is
filed as an issue and pinned, never skipped silently: the change that pins it
adds a list of the (seed, item) pairs that show the failure, and the sweep
expects exactly those, so the fix has to remove the pin. Replaying one seed
with `KBL_SWEEP_SEED` should ignore pins and fail on what it finds.
