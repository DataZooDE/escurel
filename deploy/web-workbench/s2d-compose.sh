#!/usr/bin/env bash
# `docker compose` for the S2D stack with the variables s2d-up.sh recorded in .s2d.env, e.g.
#   ./s2d-compose.sh ps          ./s2d-compose.sh logs -f demo-services          ./s2d-compose.sh down
# (after a reboot the stack usually needs `./s2d-up.sh`: a fresh demo gateway starts empty.)
set -euo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
[ -f "$HERE/.s2d.env" ] || { echo "s2d-compose: run ./s2d-up.sh first" >&2; exit 1; }
set -a; . "$HERE/.s2d.env"; set +a
exec docker compose -p "${WORKBENCH_COMPOSE_PROJECT:-escurel-web}" -f "$HERE/compose.yaml" -f "$HERE/compose.s2d.yaml" "$@"
