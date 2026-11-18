#!/usr/bin/env bash
# Shows the quick start's containers and their state. Uses the same engine and
# compose file as up.sh.
set -euo pipefail
cd "$(dirname "$0")"
KBL_QUICKSTART=1
source ./compose.sh

exec "${compose[@]}" ps --all "$@"
