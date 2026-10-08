#!/usr/bin/env bash
# Reset the S2D demo to its STARTING STATE (the three proposals open in "Awaiting you", nothing promoted):
#
#   cd deploy/web-workbench && ./s2d-reset.sh
#
# A demo gateway starts empty and the story is replayed whenever it is new, so a reset is a restart of the
# gateway-side services, in order (the runner and the services share the gateway's network namespace). No rebuild,
# no new data from the hetzner seed (use ./s2d-up.sh for that); the workbench keeps its VS Code state. The same is
# true of `docker compose restart` and of a reboot: the demo comes back in its starting state.
set -euo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
C=("$HERE/s2d-compose.sh")
"${C[@]}" restart escurel >/dev/null
"${C[@]}" restart runner demo-services >/dev/null
echo "s2d-reset: waiting for the story to be played again ..."
deadline=$(( $(date +%s) + ${S2D_UP_TIMEOUT_SECS:-600} ))
until "${C[@]}" exec -T demo-services test -f /demo/state/story.done 2>/dev/null; do
  [ "$(date +%s)" -lt "$deadline" ] || { echo "s2d-reset: timed out; see: $HERE/s2d-compose.sh logs demo-services" >&2; exit 1; }
  sleep 3
done
"${C[@]}" exec -T demo-services cat /demo/state/awaiting.txt
