#!/usr/bin/env bash
# The quick start: Felix, felix-gateway, the work tracker and the dashboard,
# built and run in containers on this machine (or pulled, with KBL_IMAGE_TAG
# set to a release such as v0.1.0). See "Quick start" in README.md.
set -euo pipefail
cd "$(dirname "$0")"
export KBL_QUICKSTART=1
./up.sh
node ./open-dashboard.mjs
