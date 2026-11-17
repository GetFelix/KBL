#!/bin/sh
# Starts as root only to read the host-owned 0600 token file (root maps to the
# host user under rootless Podman, and bypasses the mode under Docker), copies it
# to a file the tracker's own user owns, then runs the tracker as that user.
set -eu
install -m 0600 "$KBL_TOKENS_SRC" /run/kbl/tokens.json
chown kbl:kbl /run/kbl/tokens.json
KBL_TRACKER_TOKENS_FILE=/run/kbl/tokens.json
export KBL_TRACKER_TOKENS_FILE
exec setpriv --reuid=kbl --regid=kbl --clear-groups --no-new-privs kbl-tracker "$@"
