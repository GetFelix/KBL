#!/usr/bin/env bash
# Start the development stack from scratch and wait until it serves. The
# control plane keeps its state in memory, so the broker's log is reset with
# it: a log that outlived its control plane would no longer match it.
set -euo pipefail
cd "$(dirname "$0")"
source ./compose.sh

# With KBL_IMAGE_TAG the stack runs published images, so the checkout's scripts
# and record formats must be that release. Checked before anything is torn down.
if [[ -n "${KBL_QUICKSTART:-}" && -n "${KBL_IMAGE_TAG:-}" ]]; then
  tag="${KBL_IMAGE_TAG#v}"
  here="$(sed -n 's/^version = "\(.*\)"/\1/p' ../Cargo.toml | head -n1)"
  if [[ "$here" != "$tag" ]]; then
    echo "KBL_IMAGE_TAG=$KBL_IMAGE_TAG is for KBL $tag, but this checkout is $here." >&2
    echo "Check out v$tag, or unset KBL_IMAGE_TAG to build from this checkout." >&2
    exit 1
  fi
  echo "quick start: pulling ghcr.io/getfelix/kbl-tracker:$tag and kbl-dashboard:$tag (no build)"
elif [[ -n "${KBL_QUICKSTART:-}" ]]; then
  echo "quick start: building the tracker and dashboard from this checkout"
fi

"${compose[@]}" down --volumes --remove-orphans >/dev/null 2>&1 || true
rm -rf state
mkdir -p state
if [[ -n "${KBL_QUICKSTART:-}" ]]; then
  ./quickstart-tokens.sh
fi
up=(up --detach)
if [[ -n "${KBL_QUICKSTART:-}" ]]; then
  if [[ -n "${KBL_IMAGE_TAG:-}" ]]; then
    # Published images carry the version without the tag's leading v.
    tag="${KBL_IMAGE_TAG#v}"
    export KBL_TRACKER_IMAGE="ghcr.io/getfelix/kbl-tracker:$tag"
    export KBL_DASHBOARD_IMAGE="ghcr.io/getfelix/kbl-dashboard:$tag"
    "${compose[@]}" pull tracker dashboard
    up+=(--no-build)
  else
    up+=(--build)
  fi
fi
if ! "${compose[@]}" "${up[@]}"; then
  "${compose[@]}" logs >&2
  exit 1
fi

# The dashboard's proxy reaches the tracker when /v1 answers anything but a
# gateway error (it answers 401 without a token).
proxied_to_tracker() {
  local code
  code="$(curl -s -o /dev/null -w '%{http_code}' http://127.0.0.1:8081/v1/healthz 2>/dev/null)" || return 1
  [[ "$code" != 000 && "$code" != 50[234] ]]
}

# The quick start also builds the tracker and the dashboard from source,
# unless it pulls them.
tries=90
[[ -n "${KBL_QUICKSTART:-}" ]] && tries=450
for _ in $(seq 1 "$tries"); do
  if [[ -f state/seeded && -f state/finalized ]] \
    && curl -fsS http://127.0.0.1:8080/ready >/dev/null 2>&1 \
    && curl -fsS http://127.0.0.1:8787/oidc >/dev/null 2>&1 \
    && running="$("${compose[@]}" ps --status running --services 2>/dev/null)" \
    && grep -qx gateway <<<"$running" \
    && { [[ -z "${KBL_QUICKSTART:-}" ]] || {
      curl -fsS http://127.0.0.1:8790/healthz >/dev/null 2>&1 \
        && curl -fsS http://127.0.0.1:8081/ >/dev/null 2>&1 \
        && proxied_to_tracker \
        && curl -fsS http://127.0.0.1:8788/oidc >/dev/null 2>&1; }; }; then
    echo "Felix is up for tenant agents."
    echo "  control plane:  http://127.0.0.1:8443"
    echo "  broker:         127.0.0.1:5000 (QUIC), certificate at dev/state/broker-cert.pem"
    echo "  gateway:        ws://127.0.0.1:8787/ws, scope file dev/scope.toml"
    echo "  identity:       http://127.0.0.1:9400, mint a token with /token?sub=<name>&aud=kbl"
    echo "  stop:           dev/down.sh"
    exit 0
  fi
  sleep 2
done

echo "the stack did not come up" >&2
if [[ -n "${KBL_QUICKSTART:-}" ]]; then
  echo "--- tracker log ---" >&2
  "${compose[@]}" logs tracker >&2 || true
fi
"${compose[@]}" ps --all >&2
"${compose[@]}" logs >&2
exit 1
