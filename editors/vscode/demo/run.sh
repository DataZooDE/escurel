#!/usr/bin/env bash
# Start (or stop) a demo of the escurel VS Code extension: a real gateway that verifies tokens, a
# real runner, a story already played into it, and a VS Code window signed in and ready.
#
#   demo/run.sh start     build what is missing, start everything, open the window
#   demo/run.sh stop      stop it all (the window is left to you to close)
#   demo/run.sh status
#
# Evolve scenarios (optional): set ESCUREL_DEMO_EVOLVE_AGENT_BIN to an `evolve-agent` built with
# `--features synthetic-brain`, and ANOFOX_EXTENSION_DIR to a DuckDB 1.5.6 extension profile. The demo
# then starts Evolve against this gateway, runs three scripted searches (no model spend; synthetic
# data) as the demo user, and leaves one comparison page for each, ready for "Compute comparison".
#
# Binaries (override with the env vars): ESCUREL_TEST_GATEWAY_BIN, ESCUREL_RUNNER_BIN, under
# <repo>/target/release by default. Everything lives in $ESCUREL_DEMO_HOME (default
# ~/.cache/escurel-demo, on the real disk) and uses a throwaway VS Code profile, so your own
# editor settings and extensions are untouched.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
EXT="$(cd "$HERE/.." && pwd)"
REPO="$(cd "$EXT/../.." && pwd)"
HOME_DIR="${ESCUREL_DEMO_HOME:-$HOME/.cache/escurel-demo}"
GATEWAY_BIN="${ESCUREL_TEST_GATEWAY_BIN:-$REPO/target/release/escurel-test-gateway}"
RUNNER_BIN="${ESCUREL_RUNNER_BIN:-$REPO/target/release/escurel-runner}"
CODE="${ESCUREL_DEMO_CODE:-code}"

stop() {
  # The launcher's pid is not the window's: Electron forks, and its MAIN process lists
  # `--user-data-dir` and the path as separate arguments, so a pattern containing both together
  # never matches it and every restart left the previous window alive (four of them, found when a
  # stale one answered the debugging port). Match the profile path alone. The `[p]` keeps this
  # script's own command line out of the match.
  pkill -f -- "${HOME_DIR}/[p]rofile" 2>/dev/null || true
  sleep 2
  # A window that has been up for hours ignores SIGTERM.
  pkill -9 -f -- "${HOME_DIR}/[p]rofile" 2>/dev/null || true
  for f in code evolve runner gateway ratings confirmations; do
    if [ -f "$HOME_DIR/$f.pid" ]; then
      kill "$(cat "$HOME_DIR/$f.pid")" 2>/dev/null || true
      rm -f "$HOME_DIR/$f.pid"
    fi
  done
}

case "${1:-start}" in
  stop) stop; echo "demo stopped"; exit 0 ;;
  status)
    for f in gateway runner evolve code ratings confirmations; do
      if [ -f "$HOME_DIR/$f.pid" ] && kill -0 "$(cat "$HOME_DIR/$f.pid")" 2>/dev/null; then echo "$f: running"; else echo "$f: not running"; fi
    done
    exit 0 ;;
  start) ;;
  *) echo "usage: $0 start|stop|status" >&2; exit 2 ;;
esac

for bin in "$GATEWAY_BIN" "$RUNNER_BIN"; do
  [ -x "$bin" ] || { echo "missing $bin (cargo build --release -p escurel-test-support -p escurel-runner)" >&2; exit 1; }
done
if [ ! -f "$EXT/dist/extension.js" ]; then (cd "$EXT" && npm run build >/dev/null); fi

stop
rm -rf "$HOME_DIR"
mkdir -p "$HOME_DIR/workspace" "$HOME_DIR/profile/User" "$HOME_DIR/ext"

# The seed, with one placeholder resolved: the `order-lines` sql_view reads a JSON extract, and DuckDB
# resolves a relative glob against the server's cwd, so its skill page must carry an absolute path.
cp -r "$HERE/seed" "$HOME_DIR/seed"
sed -i "s|@ORDER_LINES_DIR@|$HERE/sources/order-lines|" "$HOME_DIR/seed/skills/order-lines.md"
if [ -n "${ESCUREL_DEMO_EVOLVE_AGENT_BIN:-}" ]; then ESCUREL_DEMO_EVOLVE_SEED=1; fi
if [ "${ESCUREL_DEMO_EVOLVE_SEED:-0}" = "1" ]; then
  cp "$EXT"/test/integration/seed/skills/evolve_*.md "$HOME_DIR/seed/skills/"
  cp "$EXT"/test/integration/seed/skills/plan_policy.md "$HOME_DIR/seed/skills/"
fi
# The orders and the suppliers are `instances: rows` sql_views over SAP-shaped extracts (VBAK, LFA1):
# one instance per row, no materialise step (the view is created on first read).
sed -i "s|@VBAK_DIR@|$HERE/sources/vbak|" "$HOME_DIR/seed/skills/customer-order.md"
sed -i "s|@LFA1_DIR@|$HERE/sources/lfa1|" "$HOME_DIR/seed/skills/supplier.md"

# The Source-to-Deliver (S2D) demo: data, skills and reports from the hetzner-agent-substrate seed (the
# single source), built locally by s2d/sync.sh. ESCUREL_DEMO_S2D=0 leaves it out.
S2D_DIR=""
if [ "${ESCUREL_DEMO_S2D:-1}" = "1" ]; then
  S2D_DIR="$HOME_DIR/s2d"
  ESCUREL_CLI_BIN="${ESCUREL_CLI_BIN:-$REPO/target/release/escurel}" "$HERE/s2d/sync.sh" "$S2D_DIR"
fi

# Two outside systems, as real local processes on real sockets: a REST portal (supplier ratings) and
# an MCP server (delivery confirmations). escurel reads them like any external system.
service() { # name script
  setsid nohup node "$HERE/services/$2" > "$HOME_DIR/$1.json" 2> "$HOME_DIR/$1.log" < /dev/null &
  echo $! > "$HOME_DIR/$1.pid"
  for _ in $(seq 1 40); do [ -s "$HOME_DIR/$1.json" ] && break; sleep 0.25; done
  [ -s "$HOME_DIR/$1.json" ] || { echo "$1 printed nothing; see $HOME_DIR/$1.log" >&2; exit 1; }
}
service ratings ratings-api.mjs
service confirmations confirmations-mcp.mjs
svc_port() { python3 -c "import json; print(json.loads(open('$HOME_DIR/$1.json').readline())['port'])"; }
export ESCUREL_DEMO_RATINGS_URL="http://127.0.0.1:$(svc_port ratings)"
export ESCUREL_DEMO_CONFIRMATIONS_URL="http://127.0.0.1:$(svc_port confirmations)/mcp"

# The SQL database behind `orders-db`: a real SQLite file. A tenant never names a path: an admin
# registers a credential that is a secret reference (a file under ESCUREL_SECRET_FILE_DIRS holding the
# connection string), and the operator allows the directory the database file may live in. The
# directory connectors (json_dir) read files too, so the demo's `sources/` is exposed the same way.
# A tenant's secret files live under `<dir>/<tenant>/` (the demo's tenant is `vsx`).
mkdir -p "$HOME_DIR/secrets/vsx" "$HOME_DIR/sqlite"
node "$HERE/sources/orders-db/seed.mjs" "$HOME_DIR/sqlite/orders.db" 2>/dev/null
printf '%s\n' "$HOME_DIR/sqlite/orders.db" > "$HOME_DIR/secrets/vsx/orders-db"
export ESCUREL_DEMO_ORDERS_DB_SECRET="$HOME_DIR/secrets/vsx/orders-db"

# The gateway: verifies tokens, and keeps a fresh bearer in a file (a demo outlasts a token). Its
# outbound policy is strict by default (https, public addresses only); the demo's outside systems are
# local, so loopback is opened for THIS process only.
# Extensions some S2D query pages need (see s2d/optional.py); empty unless the build is there.
INDEX_EXT=""; [ -n "$S2D_DIR" ] && [ -s "$S2D_DIR/index-extensions" ] && INDEX_EXT="$(head -1 "$S2D_DIR/index-extensions")"
# A DuckDB extension is built for ONE DuckDB version. The gateway links libduckdb.so: the copy the build
# downloaded for the version it is pinned to (target/duckdb-download/<triple>/<version>/), or whatever
# the system has, which may be another version. When an extension is to be loaded, prefer the pinned copy
# (override with ESCUREL_DEMO_LIBDUCKDB_DIR).
libduckdb_dir() {
  [ -n "${ESCUREL_DEMO_LIBDUCKDB_DIR:-}" ] && { echo "$ESCUREL_DEMO_LIBDUCKDB_DIR"; return; }
  local d
  for d in "$(dirname "$GATEWAY_BIN")"/../duckdb-download/*/*/; do
    [ -f "${d}libduckdb.so" ] && { echo "${d%/}"; return; }
  done
}
start_gateway() {
  local ld="${LD_LIBRARY_PATH:-}"
  if [ -n "$INDEX_EXT" ] && [ -n "$(libduckdb_dir)" ]; then ld="$(libduckdb_dir)${ld:+:$ld}"; fi
  LD_LIBRARY_PATH="$ld" ESCUREL_INDEX_EXTENSIONS="$INDEX_EXT" ESCUREL_EGRESS_ALLOW_LOOPBACK=1 ESCUREL_SECRET_FILE_DIRS="$HOME_DIR/secrets" ESCUREL_SQL_FILE_DIRS="$HOME_DIR/sqlite:$HERE/sources${S2D_DIR:+:$S2D_DIR/data}" setsid nohup "$GATEWAY_BIN" --tenant vsx --seed "$HOME_DIR/seed" --subject alice \
    --bearer-file "$HOME_DIR/bearer.json" > "$HOME_DIR/gateway.json" 2> "$HOME_DIR/gateway.log" < /dev/null &
  echo $! > "$HOME_DIR/gateway.pid"
  for _ in $(seq 1 120); do
    [ -s "$HOME_DIR/gateway.json" ] && return 0
    kill -0 "$(cat "$HOME_DIR/gateway.pid")" 2>/dev/null || return 1
    sleep 0.5
  done
  return 1
}
if ! start_gateway; then
  # An extension built for another DuckDB version stops the gateway at boot. The optimizer pages are an
  # extra: say so, leave them out and start again; anything else that stopped it is still an error.
  if [ -n "$INDEX_EXT" ] && grep -q "built specifically for DuckDB" "$HOME_DIR/gateway.log" 2>/dev/null; then
    echo "s2d: WARNING $INDEX_EXT was built for another DuckDB version than the gateway's ($(grep -o "this version of DuckDB is '[^']*'" "$HOME_DIR/gateway.log" | head -1)): leaving the optimizer pages out" >&2
    INDEX_EXT=""
    python3 "$HERE/s2d/optional.py" "$S2D_DIR" /nonexistent
    rm -f "$S2D_DIR/index-extensions"
    start_gateway || { echo "the gateway printed nothing; see $HOME_DIR/gateway.log" >&2; exit 1; }
  else
    echo "the gateway printed nothing; see $HOME_DIR/gateway.log" >&2; exit 1
  fi
fi

field() { python3 -c "import json,sys; print(json.loads(open('$HOME_DIR/gateway.json').readline())['$1'])"; }

# A sql_view instance is not a page you author: an admin materialises it. This is what lets the
# analysis chart (Peacock's supplier-risk-report -> query analysis_orders) read the order lines.
node "$HERE/materialise.mjs" "$HOME_DIR/gateway.json"
PORT="$(python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1",0)); print(s.getsockname()[1])')"

# The runner, MINTED mode: it signs a token per run, which is what lets the gateway tell which run
# wrote what, so the thread shows a changeset under its run.
start_runner() {
  env -u ESCUREL_RUNNER_TOKEN \
    ESCUREL_RUNNER_GATEWAY_URL="$(field gateway_url)" ESCUREL_RUNNER_TENANT="$(field tenant)" \
    ESCUREL_RUNNER_AUTH_ISSUER="$(field issuer_url)" ESCUREL_RUNNER_AUTH_KID="$(field kid)" \
    ESCUREL_RUNNER_AUTH_SIGNING_KEY="$(field signing_key)" ESCUREL_RUNNER_HARNESS="$1" \
    ESCUREL_RUNNER_LISTEN="127.0.0.1:$PORT" ESCUREL_RUNNER_LEDGER_PATH="$HOME_DIR/ledger.duckdb" \
    ESCUREL_RUNNER_POLL_INTERVAL=250ms \
    setsid nohup "$RUNNER_BIN" > "$HOME_DIR/runner.log" 2>&1 < /dev/null &
  echo $! > "$HOME_DIR/runner.pid"
}
start_runner echo

echo "playing the story (a few seconds)..."
node "$HERE/driver.mjs" "$HOME_DIR/gateway.json" "$HOME_DIR/bearer.json" > "$HOME_DIR/story.json"
# The S2D stories: three agent proposals waiting for a planner (see s2d/REHEARSAL.md).
if [ -n "$S2D_DIR" ]; then
  echo "loading the S2D demo (hetzner seed $(cat "$S2D_DIR/STAMP"))..."
  node "$HERE/s2d/seed.mjs" "$HOME_DIR/gateway.json" "$HOME_DIR/bearer.json" "$S2D_DIR" > "$HOME_DIR/s2d-story.json"
fi
if [ "${ESCUREL_DEMO_RUNNER_HARNESS:-echo}" != echo ]; then
  kill "$(cat "$HOME_DIR/runner.pid")"
  for _ in $(seq 1 100); do
    kill -0 "$(cat "$HOME_DIR/runner.pid")" 2>/dev/null || break
    sleep 0.1
  done
  PORT="$(python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1",0)); print(s.getsockname()[1])')"
  start_runner "$ESCUREL_DEMO_RUNNER_HARNESS"
fi

# The Evolve scenarios: a real Evolve service in OIDC mode against this gateway's token issuer, then
# two scripted searches and their comparison pages (see evolve-scenarios.mjs).
if [ -n "${ESCUREL_DEMO_EVOLVE_AGENT_BIN:-}" ]; then
  [ -x "$ESCUREL_DEMO_EVOLVE_AGENT_BIN" ] || { echo "missing $ESCUREL_DEMO_EVOLVE_AGENT_BIN" >&2; exit 1; }
  EVOLVE_PORT="$(python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1",0)); print(s.getsockname()[1])')"
  ISSUER="$(field issuer_url)"
  ADMIN_BEARER="$(python3 -c "import json; print(json.load(open('$HOME_DIR/bearer.json'))['admin_bearer'])")"
  ESCUREL_ENDPOINT="$(field gateway_url)" ESCUREL_TOKEN="$ADMIN_BEARER" \
    ESCUREL_OIDC_ISSUER="$ISSUER" ESCUREL_OIDC_AUDIENCE=escurel \
    ESCUREL_OIDC_JWKS_URI="$ISSUER/protocol/openid-connect/certs" \
    EVOLVE_OIDC_ISSUER="$ISSUER" EVOLVE_OIDC_AUDIENCE=escurel \
    EVOLVE_OIDC_JWKS_URI="$ISSUER/protocol/openid-connect/certs" EVOLVE_TENANT="$(field tenant)" \
    GEMINI_API_KEY=unused-scripted-demo-key EVOLVE_ALLOW_SYNTHETIC_BRAIN=1 \
    setsid nohup "$ESCUREL_DEMO_EVOLVE_AGENT_BIN" serve --addr "127.0.0.1:$EVOLVE_PORT" \
    --db "$HOME_DIR/evolve.duckdb" > "$HOME_DIR/evolve.log" 2>&1 < /dev/null &
  echo $! > "$HOME_DIR/evolve.pid"
  for _ in $(seq 1 120); do
    curl -fs "http://127.0.0.1:$EVOLVE_PORT/healthz" >/dev/null 2>&1 && break
    sleep 0.25
  done
  curl -fs "http://127.0.0.1:$EVOLVE_PORT/healthz" >/dev/null 2>&1 || { echo "Evolve did not start; see $HOME_DIR/evolve.log" >&2; exit 1; }
  export ESCUREL_DEMO_EVOLVE_ENDPOINT="http://127.0.0.1:$EVOLVE_PORT"
  # Land on the first comparison instead of the CRM story: a visitor should see the Evolve demo first.
  export ESCUREL_DEMO_OPEN_PAGE="${ESCUREL_DEMO_OPEN_PAGE:-markdown/instances/evolve_comparison/demo-assortment-vs-top-n.md}"
  echo "running the Evolve scenarios (three scripted searches)..."
  node "$HERE/evolve-scenarios.mjs" "$HOME_DIR/gateway.json" "$HOME_DIR/bearer.json" "$ESCUREL_DEMO_EVOLVE_ENDPOINT" \
    > "$HOME_DIR/evolve-scenarios.json" || { echo "the Evolve scenarios failed; see $HOME_DIR/evolve.log" >&2; exit 1; }
fi

cat > "$HOME_DIR/profile/User/settings.json" <<JSON
{
  "escurel.gatewayUrl": "$(field gateway_url)",
  "escurel.evolveEndpoint": "${ESCUREL_DEMO_EVOLVE_ENDPOINT:-}",
  "security.workspace.trust.enabled": false,
  "workbench.startupEditor": "none",
  "workbench.tips.enabled": false,
  "workbench.tree.enableStickyScroll": false,
  "telemetry.telemetryLevel": "off",
  "update.mode": "none",
  "extensions.autoUpdate": false,
  "window.restoreWindows": "none",
  "window.zoomLevel": ${ESCUREL_DEMO_ZOOM:-1},
  "window.dialogStyle": "${ESCUREL_DEMO_DIALOG_STYLE:-native}",
  "chat.disableAIFeatures": true,
  "workbench.secondarySideBar.defaultVisibility": "visible",
  "workbench.layoutControl.enabled": false,
  "workbench.welcomePage.walkthroughs.openOnInstall": false
}
JSON

# The window. ESCUREL_DEMO_* tell the bootstrap extension where the bearer and the story are.
ESCUREL_DEMO_BEARER_FILE="$HOME_DIR/bearer.json" ESCUREL_DEMO_STORY="$HOME_DIR/story.json" \
  setsid nohup "$CODE" --user-data-dir "$HOME_DIR/profile" --extensions-dir "$HOME_DIR/ext" \
  --extensionDevelopmentPath="$EXT" --extensionDevelopmentPath="$HERE/bootstrap" \
  ${ESCUREL_DEMO_CDP_PORT:+--remote-debugging-port=$ESCUREL_DEMO_CDP_PORT} ${ESCUREL_DEMO_CODE_ARGS:-} \
  --new-window "$HOME_DIR/workspace" > "$HOME_DIR/code.log" 2>&1 < /dev/null &
echo $! > "$HOME_DIR/code.pid"

echo "ready. gateway $(field gateway_url); story: $(cat "$HOME_DIR/story.json")"
echo "stop it with: $0 stop"
