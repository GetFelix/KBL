# Metrics

The tracker, ingest and `kbl-mcp` each serve Prometheus metrics. This page
lists every metric and label, how to turn the endpoint on, and what it can
and cannot show. SLOs, alert rules and runbooks are in [observability.md](observability.md).

## Turning it on

Each service has its own metrics port, off until you set the address. It is
never the API port: the tracker's API listener carries bearer tokens, and a
scrape target should not.

| Service | Flag | Environment |
|---|---|---|
| `kbl-tracker` | `--metrics-listen 127.0.0.1:9790` | `KBL_TRACKER_METRICS_LISTEN` |
| `kbl-ingest` | `--metrics-listen 127.0.0.1:9791` | `KBL_INGEST_METRICS_LISTEN` |
| `kbl-mcp` | `--metrics-listen 127.0.0.1:9792` | `KBL_MCP_METRICS_LISTEN` |

The only route is `GET /metrics`, in the Prometheus text format. The endpoint
has no authentication, so it listens on loopback only. To scrape from another
host, bind a non-loopback address and set `--metrics-allow-remote`
(`KBL_<SERVICE>_METRICS_ALLOW_REMOTE`), and restrict the port to the scraper
with a firewall or a private network. The flag is a promise the process
cannot check. Metrics are plain HTTP.

`kbl-mcp` runs once per agent and exits with its run, so give each its own
port, or leave the endpoint off and read the tracker's view of the same agents.

## What a scrape can show

Labels are route templates, tool names, the source adapter's name, team names
and fixed result words. A scrape never carries item ids, titles, comment or
issue text, agent or person names, repo names, upstream ids or tokens. A
request's route label is the template (`/v1/work/items/{item}`), not the path
it was called with. The `team` label is the team's name as configured on the
tracker, so anyone who can scrape learns which teams exist. The set is bounded by
the teams the org API can create, and nothing caps how many that is: an org
admin can create as many as it likes, so the label has as many values as the
org has teams.

## Every service

| Metric | Type | Labels | Meaning |
|---|---|---|---|
| `process_start_time_seconds` | gauge | | Unix time the process started |
| `kbl_uptime_seconds` | gauge | | Seconds since start |
| `kbl_build_info` | gauge | `service`, `version` | Always 1 |
| `process_cpu_seconds_total` | counter | | CPU time used, in whole seconds (Linux only) |
| `process_resident_memory_bytes` | gauge | | Resident memory (Linux only) |
| `process_open_fds` | gauge | | Open file descriptors (Linux only) |

## Tracker

Latency histograms use buckets from 5 ms to 60 s.

| Metric | Type | Labels | Meaning |
|---|---|---|---|
| `kbl_tracker_http_requests_total` | counter | `route`, `method`, `status` | REST calls. `route` is the matched template. Agent routes under `/v1/teams/{team}/agents/` share one label, `/v1/teams/{team}/agents/{*rest}`. A path that matches no route is not counted. `method` is GET, POST, PUT, DELETE, PATCH or `other` |
| `kbl_tracker_http_request_seconds` | histogram | `route`, `method`, `status` | Time to answer, including the wait a long-polling `/v1/work/next` asks for |
| `kbl_tracker_claims_total` | counter | `team`, `outcome` | Calls to `work_next` by result: `claimed` (a new claim), `resumed` (the agent's standing claim handed back), `empty` (nothing ready, after the wait) or `error` |
| `kbl_tracker_claim_seconds` | histogram | `team`, `outcome` | Time from `work_next` arriving to its answer. For `claimed` this is the time to a claim; for `empty` it is the wait the agent asked for |
| `kbl_tracker_completions_total` | counter | `team` | `done` records written. A replayed call counts nothing |
| `kbl_tracker_releases_total` | counter | `team` | `released` records written |
| `kbl_tracker_give_ups_total` | counter | `team` | `gave_up` records written |
| `kbl_tracker_fold_applied_offset` | gauge | `team` | Offset of the last record the tracker's fold applied |
| `kbl_tracker_stream_tip_offset` | gauge | `team` | Felix's committed tip of `work.events`, read every few seconds |
| `kbl_tracker_fold_lag_records` | gauge | `team` | Committed records past what the fold has seen. Zero while this tracker is the only writer; above zero for as long as another writer's records are not folded |
| `kbl_tracker_fold_rebuilds_total` | counter | `team` | Rebuilds of the fold from the log |
| `kbl_tracker_fold_rebuild_seconds` | histogram | `team` | Time to read the log and fold it |
| `kbl_tracker_tip_read_failures_total` | counter | `team` | Reads of the stream tip that failed, so the gauges above went stale |

The offsets are Felix offsets. The tip can pass the applied offset by more
than one when Felix skips offsets that hold no record, so read
`kbl_tracker_fold_lag_records`, not the difference.

## Ingest

`source` is the adapter's name, `github` today. Lag histograms use buckets
from 1 second to 1 day.

| Metric | Type | Labels | Meaning |
|---|---|---|---|
| `kbl_ingest_polls_total` | counter | `source`, `result` | Polls by result: `ok`, `rate_limited` or `failed` |
| `kbl_ingest_poll_seconds` | histogram | `source` | Time a poll took |
| `kbl_ingest_errors_total` | counter | `source`, `kind` | Failed polls by kind: `rate_limited` or `failed` |
| `kbl_ingest_lag_seconds` | histogram | `source` | For each issue or comment recorded, the time from the upstream `updated_at` to the moment ingest finished with it. Not recorded on a first sync, after the cursors are lost, or on a forced reconcile, which read the whole backlog |
| `kbl_ingest_last_success_timestamp_seconds` | gauge | `source` | Unix time the last good poll finished |
| `kbl_ingest_denied_repos` | gauge | `source` | Repos the source could not read in the last poll |
| `kbl_ingest_webhooks_total` | counter | `result` | Webhook deliveries: `woke` (woke the poll), `coalesced` (inside the minimum gap), `duplicate` or `unauthorized` |

## kbl-mcp

`tool` is one of the server's tool names. A call to a name that is not a tool
is counted as `unknown`.

| Metric | Type | Labels | Meaning |
|---|---|---|---|
| `kbl_mcp_tool_calls_total` | counter | `tool`, `outcome` | Tool calls by result: `ok`, `error` (the tool answered with an error) or `protocol_error` (the call was refused before or by the tool, such as bad arguments) |
| `kbl_mcp_tool_seconds` | histogram | `tool` | Time a tool took |
| `kbl_mcp_tool_errors_total` | counter | `tool`, `outcome` | The calls above that were not `ok` |

## Queries

```promql
# Claim latency, 95th percentile, over five minutes
histogram_quantile(0.95, sum by (le) (rate(kbl_tracker_claim_seconds_bucket{outcome="claimed"}[5m])))

# 5xx rate by route
sum by (route) (rate(kbl_tracker_http_requests_total{status=~"5.."}[5m]))

# Ingest is behind
time() - kbl_ingest_last_success_timestamp_seconds > 300

# The fold is not keeping up with Felix
kbl_tracker_fold_lag_records > 0
```
