#!/usr/bin/env bash
# The quick start: Felix, felix-gateway, the work tracker and the dashboard,
# built and run in containers on this machine. See "Quick start" in README.md.
set -euo pipefail
cd "$(dirname "$0")"
export KBL_QUICKSTART=1
./up.sh
node ./open-dashboard.mjs
