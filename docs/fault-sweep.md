# Fault sweep

The tracker's claim path and ingest each run under a seeded sweep of faults,
and check the log after every step. A seed is one deterministic run, so a
failure replays exactly.

| Sweep | Where | Faults | Invariants |
| --- | --- | --- | --- |
| Claim path | `tracker/src/tracker/tests/sweep.rs` | the tracker dies after some of its writes and restarts from the log; a lost publish reply; another writer takes the offset, with a claim or release of its own (a failover; on half the seeds a refusal burns the offset, as on a Raft shard); a replaced tracker that keeps serving until it is fenced; Felix hands a ready record out again, acked or not; a ready lane that fails to poll; leases that run out | a claimed item is never already held; a claim is handed to one agent, and an attempt to a second only after the first hold ended; no attempt is done twice; the log folds from nothing; a dismissed root stays dismissed and unheld; an imported item stays; an accepted item ends done, closed or waiting for a person |
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

A bug the sweep finds on main is filed as an issue and pinned in the sweep
until it is fixed, never silently. A pin lists the (seed, item) pairs that
show the failure and expects exactly those, so the fix removes the pin in the
same change. Replaying one seed with `KBL_SWEEP_SEED` ignores pins and fails
on what it finds. Neither sweep has a pin today.
