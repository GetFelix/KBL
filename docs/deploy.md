# Deploying KBL

This page covers running KBL on Kubernetes with the published images and the
Helm chart in `deploy/helm/kbl`. For a single-machine trial, use the
[quick start](../README.md#quick-start) instead.

The chart installs the work tracker, the dashboard and, if you ask for it, the
ingest worker. It does **not** install Felix. KBL keeps all its state in Felix,
so install Felix first, from Felix's own Helm chart, and give the tracker its
addresses.

## Images

A version tag publishes two multi-arch images (amd64 and arm64) to GitHub's
registry:

| Image | Holds | Runs as |
| --- | --- | --- |
| `ghcr.io/getfelix/kbl-tracker:<version>` | `kbl-tracker` and `kbl-ingest` | uid 10001 |
| `ghcr.io/getfelix/kbl-dashboard:<version>` | the dashboard behind nginx, on port 8080 | uid 101 (nginx-unprivileged) |

`kbl-ingest` ships in the tracker image because both come from one build and
one version, and the worker is only ever run next to a tracker of the same
version. Start it by overriding the command: the image's default entrypoint
starts the tracker.

Each image carries build provenance. Check one with:

```bash
gh attestation verify oci://ghcr.io/getfelix/kbl-tracker:<version> -R GetFelix/KBL
```

Docker and Podman both run them. The `<version>-amd64` and `<version>-arm64`
tags are the per-architecture images the joined tag points at; pull the joined
tag.

## What Felix must be set up for

KBL needs an acknowledged record on `work.events` to stay put. The tracker
creates `work.events` as a durable stream with one copy, and that is safe only
when the brokers run with `FELIX_ACK_ON_COMMIT=true` and
`FELIX_DURABLE_FSYNC_MODE=on_commit`. For more than one copy, create the stream
yourself before the first start, with consistency `Quorum` (or on Felix's Raft
engine). The tracker refuses to serve any other setting. The reasoning is in
[What Felix must guarantee](work-tracker.md#what-felix-must-guarantee). The
chart cannot check this, so its install notes repeat it.

The tracker also needs an identity: either an identity provider it can ask for
a token (`felix.idp`), or a ready ID token in a Secret
(`felix.idTokenSecret`). A fixed ID token is not renewed, so it only suits
short-lived tests.

## Install

Create the Secrets first. Nothing secret goes in `values.yaml`.

```bash
# The token file: agents and people, as in docs/work-tracker.md.
kubectl create secret generic kbl-tracker-tokens --from-file=tokens.json

# The CA that signed the broker's certificate. It is public, so a ConfigMap.
kubectl create configmap felix-ca --from-file=ca.crt=broker-cert.pem

helm install kbl deploy/helm/kbl \
  --set felix.controlPlane=https://controlplane.felix.svc:8443 \
  --set felix.broker=broker.felix.svc:5000 \
  --set felix.brokerName=broker.felix.svc \
  --set felix.idp=https://idp.example.com \
  --set felix.ca.existingConfigMap=felix-ca \
  --set tracker.tokensSecret.name=kbl-tracker-tokens
```

The chart refuses to render without the control plane, the broker, the CA, an
identity source and the tokens Secret. `examples/values-production.yaml` shows
a fuller set of values: an Ingress with TLS, network policies, metrics and
ingest.

### The ingest worker

Set `ingest.enabled=true` and give it two things from Secrets: its JSON config
(`ingest.configSecret`) and the credentials the config names, as environment
variables (`ingest.secretEnv`). The config's `tracker.url` is the tracker's
Service, `http://<release>-kbl-tracker:8790`. A GitHub App key must be mode 400
or 600, which a mounted Secret is not, so set `ingest.privateKey` and point
`private_key_file` at `/run/kbl/app.pem`; see
[Running the ingest worker](work-tracker.md#running-the-ingest-worker). Only
one ingest runs: two would import every issue twice.

### The dashboard and its gateway

The dashboard reads `work.events` through a felix-gateway, from the browser. The
gateway is not part of this chart. Run one against the same Felix, with a scope
for the `work.events` stream (the quick start's `dev/scope-work.toml` is an
example), expose it over TLS, and open the dashboard with its address:
`?gateway=wss://<gateway>/ws&room=...&token=<ID token>`. The dashboard's own
nginx forwards `/v1` to the tracker.

The page sends the ID token only to the gateway its own link named. A link with
`?gateway=` and no token keeps the gateway the tab signed in with, and a gateway
address must be `wss://` (`ws://` is accepted for localhost only). The gateway
host is shown beside the connection status, so a person can see where the token
goes. Every container in the chart has an `ephemeral-storage` request and limit
in `values.yaml`.

## How the pods are locked down

- Every container is non-root with `runAsNonRoot`, a read-only root file
  system, no privilege escalation, all capabilities dropped and the runtime's
  default seccomp profile. Scratch space is an `emptyDir`.
- No pod mounts a service account token.
- The tracker's token file must be mode 400 or 600 and readable by uid 10001.
  A mounted Secret is owned by root, so a one-shot init container copies it to
  a memory volume with the right owner and mode. That init container runs as
  root with only `CHOWN` and a read-only file system, and exits. It is the one
  root process in the release, so the namespace must allow it: the `baseline`
  Pod Security level does, `restricted` does not.
- Secrets reach the pods as files or `secretKeyRef` variables. They never pass
  through chart values.
- `networkPolicy.enabled` limits who may reach each pod: the tracker takes
  traffic from the dashboard, ingest and (when `ingress.trackerHost` is set)
  the ingress controller, and the dashboard only from the ingress controller.
  `networkPolicy.egress.enabled` also limits where the tracker and ingest may
  connect; list Felix's and the identity provider's addresses in `felixCidrs`.
  Network policies need a CNI that enforces them.
- The tracker listens on plain HTTP with bearer tokens, so the chart sets
  `KBL_TRACKER_BEHIND_TLS_PROXY`. Terminate TLS at the Ingress.

## Why one tracker

The tracker is the single writer of `work.events`. The chart runs exactly one
replica and replaces it with `Recreate`, so an upgrade stops the old pod before
the new one starts. Running a standby that takes over is tracked in
[issue 124](https://github.com/GetFelix/KBL/issues/124).

## Metrics

`tracker.metrics.enabled` and `ingest.metrics.enabled` open the metrics ports
(9790 and 9791). They have no authentication and listen on every pod address,
so turn on `networkPolicy` and allow only your scraper with
`networkPolicy.metricsFromNamespaceLabels`; without it any pod can read them. See [metrics.md](metrics.md).

## Upgrading

Upgrade in the order [upgrades.md](upgrades.md) gives, then
`helm upgrade` with the new image tag. The chart's version and `appVersion`
match the KBL release, and the default image tag is `appVersion`.

## What the chart does not do

- Install Felix, a felix-gateway or an identity provider.
- Run more than one tracker.
- Back up the log; see [backup-restore.md](backup-restore.md).
