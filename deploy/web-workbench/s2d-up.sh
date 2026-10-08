#!/usr/bin/env bash
# Brings up the web workbench WITH the Source-to-Deliver demo (hetzner seed + optimizer pages, DuckDB 1.5.6).
#
#   cd deploy/web-workbench && ./s2d-up.sh          # (re)seeds the demo from scratch, keeps the workbench's own data
#
# On the HOST (nothing from the hetzner repo is committed here):
#   1. editors/vscode/demo/s2d/sync.sh snapshots hetzner-agent-substrate origin/main (read-only), migrates a COPY
#      type: -> kind:, builds the parquet, applies the demo overlay -> $ESCUREL_WEB_S2D_DIR
#   2. the optimizer extension build for the gateway's DuckDB version is picked by its footer (optional.py); with
#      none, those query pages are left out with a warning
# Then compose runs the base file + compose.s2d.yaml. The gateway-side volumes are recreated (a replay on old data
# would duplicate the story); the workbench's own volume (VS Code settings) is kept.
#
# Environment: ESCUREL_WEB_S2D_DIR (default ~/.cache/escurel-web-s2d), ESCUREL_WEB_PROJECT (compose project,
# default the one in compose.yaml), ESCUREL_CLI_BIN (an `escurel` binary for migrate-kind-files), ESCUREL_DEMO_S2D_REPO
# (the hetzner checkout). The .env next to compose.yaml (WORKBENCH_PASSWORD ...) is read by compose, never by this script.
set -euo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$(cd "$HERE/../.." && pwd)"
OUT="${ESCUREL_WEB_S2D_DIR:-$HOME/.cache/escurel-web-s2d}"
PROJECT_ARGS=()
PROJECT="escurel-web"
if [ -n "${ESCUREL_WEB_PROJECT:-}" ]; then PROJECT="$ESCUREL_WEB_PROJECT"; PROJECT_ARGS=(-p "$PROJECT"); fi
COMPOSE=(docker compose "${PROJECT_ARGS[@]}" -f "$HERE/compose.yaml" -f "$HERE/compose.s2d.yaml")

if [ -z "${ESCUREL_CLI_BIN:-}" ]; then
  for c in "$REPO/target/release/escurel" "$HOME/wt-escurel/s2d/target/release/escurel" "$HOME/Projects/datazoo/escurel/target/release/escurel"; do
    if [ -x "$c" ]; then ESCUREL_CLI_BIN="$c"; break; fi
  done
fi
[ -x "${ESCUREL_CLI_BIN:-}" ] || { echo "s2d-up: no escurel CLI found (set ESCUREL_CLI_BIN; it only rewrites type: -> kind: in a copy)" >&2; exit 1; }
export ESCUREL_CLI_BIN

# The DuckDB the gateway image is built with decides which optimizer build can be loaded.
DUCKDB="$(sed -n 's/^ARG DUCKDB_VERSION=v\{0,1\}\([0-9.]*\).*/\1/p' "$REPO/Dockerfile" | head -1)"
echo "s2d-up: gateway DuckDB ${DUCKDB:-unknown}"
ESCUREL_DEMO_DUCKDB_VERSION="$DUCKDB" "$REPO/editors/vscode/demo/s2d/sync.sh" "$OUT"
# The gateway container runs as uid 65532: it must be able to read the parquet.
chmod -R a+rX "$OUT"

export S2D_DIR="$OUT"
# Without an optimizer build the mount still needs a directory: an empty one inside the S2D dir.
mkdir -p "$OUT/.noext"
S2D_INDEX_EXT=""; S2D_EXT_DIR="$OUT/.noext"; S2D_ALLOW_UNSIGNED=false
if [ -s "$OUT/index-extensions" ]; then
  S2D_INDEX_EXT="$(head -1 "$OUT/index-extensions")"
  S2D_EXT_DIR="$(dirname "$S2D_INDEX_EXT")"
  S2D_ALLOW_UNSIGNED=true
  chmod a+rX "$S2D_EXT_DIR" "$S2D_INDEX_EXT" 2>/dev/null || true
fi
export S2D_INDEX_EXT S2D_EXT_DIR S2D_ALLOW_UNSIGNED

cd "$HERE"
echo "s2d-up: rebuilding the gateway side from scratch (the workbench's own data is kept)"
"${COMPOSE[@]}" rm -sf escurel runner demo-init demo-services >/dev/null 2>&1 || true
for v in demo-data runner-data escurel-data; do docker volume rm -f "${PROJECT}_$v" >/dev/null 2>&1 || true; done
"${COMPOSE[@]}" up --build -d

echo "s2d-up: waiting for the workbench and for the story to be played (the first build can take long) ..."
deadline=$(( $(date +%s) + ${S2D_UP_TIMEOUT_SECS:-1500} ))
while :; do
  if "${COMPOSE[@]}" exec -T demo-services test -f /demo/state/story.done 2>/dev/null; then
    echo "s2d-up: story played"; break
  fi
  [ "$(date +%s)" -lt "$deadline" ] || { echo "s2d-up: timed out waiting for the story; see: ${COMPOSE[*]} logs demo-services" >&2; exit 1; }
  sleep 5
done
"${COMPOSE[@]}" ps --format 'table {{.Name}}\t{{.State}}\t{{.Health}}'
