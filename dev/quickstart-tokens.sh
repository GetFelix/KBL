#!/usr/bin/env bash
# Writes the tracker's token file for the quick start: one person ("me"),
# agents agent1 to agent8 and one ingest worker, each with a random token. Mode 0600, as the
# tracker requires. The tokens live only in dev/state, which up.sh recreates.
set -euo pipefail
cd "$(dirname "$0")"
umask 077
token() { head -c 32 /dev/urandom | od -An -tx1 | tr -d ' \n'; }
{
  printf '{"people":{"%s":"me"},"agents":{' "$(token)"
  for n in 1 2 3 4 5 6 7 8; do
    [[ $n -gt 1 ]] && printf ','
    printf '"%s":"agent%s"' "$(token)" "$n"
  done
  printf '},"ingest":{"%s":"ingest"}}\n' "$(token)"
} > state/tracker-tokens.json
chmod 600 state/tracker-tokens.json
