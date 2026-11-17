# Observability

What to watch, what "healthy" means, and what to do when an alert fires. The
metrics are listed in [metrics.md](metrics.md). This page turns them into
service level objectives (SLOs), alert rules and runbooks.

The targets below are proposals. Nothing in KBL enforces them, and they have
not been measured against a production load. Change them to fit your
deployment, then keep the alert thresholds in step.

The [traces](#traces) section covers spans, which show where one slow call
spent its time.

## Traces

The tracker, `kbl-ingest` and `kbl-mcp` can export OpenTelemetry traces over
OTLP/HTTP. They are off until an endpoint is set. With none set, no exporter
or subscriber is installed, spans are skipped where they are created, and no
trace headers are read or sent.

| Variable | Effect |
|---|---|
| `OTEL_EXPORTER_OTLP_ENDPOINT` or `OTEL_EXPORTER_OTLP_TRACES_ENDPOINT` | Turns export on. The first is a base URL (`http://collector:4318`); spans go to its `/v1/traces`. |
| `OTEL_EXPORTER_OTLP_HEADERS`, `OTEL_EXPORTER_OTLP_TIMEOUT` | Passed to the exporter, as the OpenTelemetry specification defines them. |
| `OTEL_SERVICE_NAME` | Overrides the service name (default `kbl-tracker`, `kbl-ingest` or `kbl-mcp`). |
| `OTEL_TRACES_SAMPLER`, `OTEL_TRACES_SAMPLER_ARG` | The sampler; the default keeps every trace unless the caller's sampled flag says otherwise. |
| `OTEL_SDK_DISABLED=true`, `OTEL_TRACES_EXPORTER=none` | Off, even with an endpoint set. |

Only the `http/protobuf` protocol is built in. A collector that is down costs
the services nothing but dropped spans; the last batch is flushed when the
process exits normally.

### Spans

| Span | Where | Attributes |
|---|---|---|
| `GET /v1/work/items/{item}` (`http.request`) | Every tracker route | `http.route`, `http.request.method`, `http.response.status_code` |
| `tracker.claim` | Inside the request that claims work | `team`, `outcome` (`claimed`, `resumed`, `empty`, `error`) |
| `tracker.fold.apply` | A record the tracker applies before writing it | `team`, `record.kind` |
| `tracker.fold.rebuild` | Rebuilding the fold from `work.events` | `team` |
| `ingest.poll` | One poll of a source | `source`, `reconcile`, `outcome` (`ok`, `failed`) |
| `ingest.write_back` | The write-back step of a poll | `source` |
| `mcp.tool` | One tool call on `kbl-mcp` | `tool` (`unknown` for a name outside the tool list), `outcome` |
| `tracker.request` | A call from `kbl-mcp` or `kbl-ingest` to the tracker | `http.route`, `http.response.status_code` |

A request span is named by method and route template, so the span for an
item is `GET /v1/work/items/{item}` whatever the item.

`kbl-mcp` and `kbl-ingest` send a W3C `traceparent` header on each call to the
tracker, and the tracker makes its request span a child of it. One tool call
or poll is then a single trace from the agent's tool to the fold. The
`kbl-mcp` keep-alive that extends a held claim runs in the background and
starts its own traces. The tracker does not call Felix with trace headers, so a
trace ends at the tracker.

### Attribute policy

Span attributes follow the same rule as metric labels: a trace must be safe to
hand to anyone who can read the collector.

- Allowed: route templates, tool names, source kinds, result words, record
  kinds, status codes, and the team name (the metrics already carry it).
- Never: item ids, item or comment text, titles, repo or project names, agent
  or person names, tokens or other credentials, request or response bodies,
  query strings, or the raw path of a request.
- A value that comes from a caller (a method, a tool name) is mapped to a
  fixed set before it becomes an attribute, as for metrics.
- Incoming `traceparent` is joined only when the request carries a token the
  tracker accepts. A request with no token or a wrong one (including
  `/healthz`) gets a root span, so a caller cannot force its spans to be
  sampled or choose their trace. Those root spans follow the tracker's own
  sampler (`OTEL_TRACES_SAMPLER`); the default records them, so set a ratio
  sampler if unauthenticated traffic is a volume concern. The tracker never
  copies `tracestate` or any other request header into a span.

A new span or attribute follows this list, and its test checks that no
secret reaches it.

## SLOs

| SLO | Signal | Proposed target |
|---|---|---|
| Claim latency | `kbl_tracker_claim_seconds{outcome="claimed"}` | 95% of claims answered within 1 second, over 30 days |
| Ingest lag | `kbl_ingest_lag_seconds`, `kbl_ingest_last_success_timestamp_seconds` | 95% of upstream changes recorded within 5 minutes, and a good poll at least every 5 minutes |
| Dashboard freshness | `kbl_tracker_fold_lag_records`, `kbl_tracker_tip_read_failures_total` | Fold lag is zero for 99% of scrapes, over 30 days |

Notes on each signal:

- **Claim latency** counts only `claimed`. An `empty` claim spends the wait the
  agent asked for, so it says nothing about speed.
- **Ingest lag** is not recorded on a first sync or a forced reconcile, so a
  backlog does not bend the histogram. The last-success gauge covers the case
  where polls stop altogether and the histogram has nothing to say.
- **Dashboard freshness** has no metric of its own. The dashboard folds
  `work.events` in the browser, so what the server can measure is whether the
  tracker's own fold has caught up with Felix. A tracker that is behind is
  stale for everyone who reads from it. Lag stays above zero for as long as
  another writer's records are unfolded, so a short spike is normal; the alert
  waits for it to persist.

## Alert rules

```yaml
groups:
  - name: kbl
    rules:
      - alert: KblClaimLatencyHigh
        expr: |
          histogram_quantile(0.95,
            sum by (le) (rate(kbl_tracker_claim_seconds_bucket{outcome="claimed"}[10m]))) > 1
        for: 10m
        labels: {severity: page}
        annotations:
          runbook: "docs/observability.md#claim-latency-high"

      - alert: KblTrackerErrors
        expr: |
          sum by (route) (rate(kbl_tracker_http_requests_total{status=~"5.."}[5m]))
            / sum by (route) (rate(kbl_tracker_http_requests_total[5m])) > 0.05
        for: 10m
        labels: {severity: page}
        annotations:
          runbook: "docs/observability.md#tracker-errors"

      - alert: KblIngestStalled
        expr: time() - kbl_ingest_last_success_timestamp_seconds > 600
        for: 5m
        labels: {severity: page}
        annotations:
          runbook: "docs/observability.md#ingest-stalled"

      - alert: KblIngestLagHigh
        expr: |
          histogram_quantile(0.95,
            sum by (le) (rate(kbl_ingest_lag_seconds_bucket[30m]))) > 300
        for: 30m
        labels: {severity: ticket}
        annotations:
          runbook: "docs/observability.md#ingest-lag-high"

      - alert: KblFoldBehind
        expr: kbl_tracker_fold_lag_records > 0
        for: 10m
        labels: {severity: page}
        annotations:
          runbook: "docs/observability.md#fold-behind"

      - alert: KblTipUnreadable
        expr: increase(kbl_tracker_tip_read_failures_total[10m]) > 3
        labels: {severity: ticket}
        annotations:
          runbook: "docs/observability.md#tip-unreadable"

      - alert: KblMcpToolErrors
        expr: |
          sum by (tool) (rate(kbl_mcp_tool_errors_total[10m]))
            / sum by (tool) (rate(kbl_mcp_tool_calls_total[10m])) > 0.1
        for: 15m
        labels: {severity: ticket}
        annotations:
          runbook: "docs/observability.md#mcp-tool-errors"
```

## Runbooks

### Claim latency high

Claims are slow: agents wait for work they could already have.

1. Check `kbl_tracker_http_request_seconds` for `/v1/work/next` against other
   routes. If every route is slow, the tracker or Felix is the problem, not
   claiming.
2. Check `process_resident_memory_bytes` and CPU on the tracker. A tracker
   that is rebuilding its fold (`kbl_tracker_fold_rebuilds_total` rising) is
   busy reading the log; claims wait on it.
3. Check Felix's own health and the network between the tracker and the
   broker. A slow append is a slow claim.
4. If a team has a very large log, rebuild time grows with it. Restarting the
   tracker repeats the rebuild, so avoid restart loops.

### Tracker errors

More than 5% of calls on a route return 5xx.

1. Find the route in the alert label and the status in
   `kbl_tracker_http_requests_total`.
2. Read the tracker log for the same window. A 5xx on a write usually means
   Felix refused or timed out the append.
3. If only one team is affected, check that team's streams and credentials.
4. If Felix is down, the tracker cannot write. Restore Felix first; the
   tracker recovers on its own once appends succeed.

### Ingest stalled

No poll has succeeded for ten minutes.

1. Look at `kbl_ingest_polls_total` by `result`. `rate_limited` means the
   upstream is throttling: wait for the window to reset, or lower the poll
   rate. `failed` means look at the ingest log.
2. Check the credential. An expired or revoked token fails every poll.
   `kbl_ingest_denied_repos` above zero means the token cannot read some
   repos.
3. If the process is up but the gauge is flat, check it can reach the
   tracker and the upstream.
4. If ingest was down for a while, the next poll catches up. Lag is not
   recorded for that catch-up, so the lag alert stays quiet.

### Ingest lag high

Polls succeed, but changes take over five minutes to land.

1. Compare the poll interval with `kbl_ingest_poll_seconds`. If a poll takes
   longer than the interval, the source has outgrown it.
2. Check `kbl_ingest_webhooks_total`. Webhooks wake the poll early; if they
   are `unauthorized`, the secret is wrong and ingest falls back to the
   interval.
3. Check for `rate_limited` results spread over time. Steady throttling
   shows up as lag before it shows up as a stall.

### Fold behind

The tracker has not applied records Felix has committed.

1. A short gap is normal while another writer's records arrive. The alert
   fires after ten minutes, so this is a real gap.
2. Check `kbl_tracker_tip_read_failures_total`. If it is rising, the gauges
   are stale and the lag may be wrong; fix the tip read first.
3. Check `kbl_tracker_fold_rebuilds_total` and `kbl_tracker_fold_rebuild_seconds`.
   A rebuild in progress explains the gap.
4. If the fold sees a record it cannot read, it stops rather than guessing.
   The tracker log says which record. See [upgrades.md](upgrades.md) for the
   upgrade order; an older tracker next to a newer writer looks like this.
5. Restart the tracker only after you know why. A restart rebuilds the fold
   from the log and will hit the same record.

### Tip unreadable

The tracker cannot read Felix's committed tip, so the lag gauges are stale.

1. Check the tracker log for the error on the read.
2. Check the broker and the credential the tracker uses.
3. Until this clears, treat `kbl_tracker_fold_lag_records` as unknown, not
   as zero.

### MCP tool errors

A tool on `kbl-mcp` fails more than 10% of the time.

1. Split by `outcome`. `protocol_error` points at the caller: bad arguments or
   an agent on an older tool schema. `error` means the tool ran and the
   tracker answered with an error.
2. For `error`, check the tracker alerts above: the tool is usually passing
   on a tracker failure.
3. Check that the token `kbl-mcp` uses is still valid.
