#!/usr/bin/env bash
# Stop the development stack and delete its data.
set -euo pipefail
cd "$(dirname "$0")"
source ./compose.sh

"${compose[@]}" down --volumes --remove-orphans
rm -rf state
