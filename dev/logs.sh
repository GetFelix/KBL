#!/usr/bin/env bash
# Logs of the quick start's services: dev/logs.sh [service ...]. Follows with -f
# when FOLLOW=1. Uses the same engine and compose file as up.sh.
set -euo pipefail
cd "$(dirname "$0")"
KBL_QUICKSTART=1
source ./compose.sh

args=(logs --tail 200)
[[ -n "${FOLLOW:-}" ]] && args+=(--follow)
exec "${compose[@]}" "${args[@]}" "$@"
