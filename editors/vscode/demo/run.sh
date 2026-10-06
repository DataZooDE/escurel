#!/usr/bin/env bash
# Start (or stop) a demo of the escurel VS Code extension: a real gateway that verifies tokens, a
# real runner, a story already played into it, and a VS Code window signed in and ready.
#
#   demo/run.sh start     build what is missing, start everything, open the window
#   demo/run.sh stop      stop it all (the window is left to you to close)
#   demo/run.sh status
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
  for f in code runner gateway ratings confirmations; do
    if [ -f "$HOME_DIR/$f.pid" ]; then
      kill "$(cat "$HOME_DIR/$f.pid")" 2>/dev/null || true
      rm -f "$HOME_DIR/$f.pid"
    fi
  done
}

case "${1:-start}" in
  stop) stop; echo "demo stopped"; exit 0 ;;
  status)
    for f in gateway runner code ratings confirmations; do
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
# The orders and the suppliers are `instances: rows` sql_views over SAP-shaped extracts (VBAK, LFA1):
# one instance per row, no materialise step (the view is created on first read).
sed -i "s|@VBAK_DIR@|$HERE/sources/vbak|" "$HOME_DIR/seed/skills/customer-order.md"
sed -i "s|@LFA1_DIR@|$HERE/sources/lfa1|" "$HOME_DIR/seed/skills/supplier.md"

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
ESCUREL_EGRESS_ALLOW_LOOPBACK=1 ESCUREL_SECRET_FILE_DIRS="$HOME_DIR/secrets" ESCUREL_SQL_FILE_DIRS="$HOME_DIR/sqlite:$HERE/sources" setsid nohup "$GATEWAY_BIN" --tenant vsx --seed "$HOME_DIR/seed" --subject alice \
  --bearer-file "$HOME_DIR/bearer.json" > "$HOME_DIR/gateway.json" 2> "$HOME_DIR/gateway.log" < /dev/null &
echo $! > "$HOME_DIR/gateway.pid"
for _ in $(seq 1 120); do [ -s "$HOME_DIR/gateway.json" ] && break; sleep 0.5; done
[ -s "$HOME_DIR/gateway.json" ] || { echo "the gateway printed nothing; see $HOME_DIR/gateway.log" >&2; exit 1; }

field() { python3 -c "import json,sys; print(json.loads(open('$HOME_DIR/gateway.json').readline())['$1'])"; }

# A sql_view instance is not a page you author: an admin materialises it. This is what lets the
# analysis chart (Peacock's supplier-risk-report -> query analysis_orders) read the order lines.
node "$HERE/materialise.mjs" "$HOME_DIR/gateway.json"
PORT="$(python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1",0)); print(s.getsockname()[1])')"

# The runner, MINTED mode: it signs a token per run, which is what lets the gateway tell which run
# wrote what, so the thread shows a changeset under its run.
env -u ESCUREL_RUNNER_TOKEN \
  ESCUREL_RUNNER_GATEWAY_URL="$(field gateway_url)" ESCUREL_RUNNER_TENANT="$(field tenant)" \
  ESCUREL_RUNNER_AUTH_ISSUER="$(field issuer_url)" ESCUREL_RUNNER_AUTH_KID="$(field kid)" \
  ESCUREL_RUNNER_AUTH_SIGNING_KEY="$(field signing_key)" ESCUREL_RUNNER_HARNESS=echo \
  ESCUREL_RUNNER_LISTEN="127.0.0.1:$PORT" ESCUREL_RUNNER_LEDGER_PATH="$HOME_DIR/ledger.duckdb" \
  ESCUREL_RUNNER_POLL_INTERVAL=250ms \
  setsid nohup "$RUNNER_BIN" > "$HOME_DIR/runner.log" 2>&1 < /dev/null &
echo $! > "$HOME_DIR/runner.pid"

echo "playing the story (a few seconds)..."
node "$HERE/driver.mjs" "$HOME_DIR/gateway.json" "$HOME_DIR/bearer.json" > "$HOME_DIR/story.json"

cat > "$HOME_DIR/profile/User/settings.json" <<JSON
{
  "escurel.gatewayUrl": "$(field gateway_url)",
  "security.workspace.trust.enabled": false,
  "workbench.startupEditor": "none",
  "workbench.tips.enabled": false,
  "workbench.tree.enableStickyScroll": false,
  "telemetry.telemetryLevel": "off",
  "update.mode": "none",
  "extensions.autoUpdate": false,
  "window.restoreWindows": "none",
  "window.zoomLevel": ${ESCUREL_DEMO_ZOOM:-1},
  "chat.disableAIFeatures": true,
  "workbench.secondarySideBar.defaultVisibility": "visible",
  "workbench.layoutControl.enabled": false,
  "workbench.welcomePage.walkthroughs.openOnInstall": false
}
JSON

# The calm window is the demo's default (ESCUREL_DEMO_FOCUS=0 keeps the classic IDE look, which is what the
# end-to-end tests of the individual views run in). The stock Explorer / Search / Source Control / Run /
# Extensions icons cannot be hidden by a setting: VS Code keeps which activity-bar icons are pinned in its
# state database, so a throwaway profile is given one in which they are not. The shipped Focus mode does not
# do this (it does not own a person's profile); it only moves the activity bar to the top.
FOCUS="${ESCUREL_DEMO_FOCUS:-1}"
if [ "$FOCUS" != "0" ]; then
  mkdir -p "$HOME_DIR/profile/User/globalStorage"
  python3 - "$HOME_DIR/profile/User/globalStorage/state.vscdb" <<'PY'
import json, sqlite3, sys
db = sqlite3.connect(sys.argv[1])
db.execute("CREATE TABLE IF NOT EXISTS ItemTable (key TEXT UNIQUE ON CONFLICT REPLACE, value BLOB)")
stock = ["explorer", "search", "scm", "debug", "remote", "extensions"]
pins = [{"id": f"workbench.view.{v}", "pinned": False, "visible": False, "order": i} for i, v in enumerate(stock)]
db.execute("INSERT INTO ItemTable VALUES ('workbench.activity.pinnedViewlets2', ?)", (json.dumps(pins),))
db.commit()
PY
fi

# The window. ESCUREL_DEMO_* tell the bootstrap extension where the bearer and the story are.
ESCUREL_DEMO_FOCUS="$FOCUS" ESCUREL_DEMO_BEARER_FILE="$HOME_DIR/bearer.json" ESCUREL_DEMO_STORY="$HOME_DIR/story.json" \
  setsid nohup "$CODE" --user-data-dir "$HOME_DIR/profile" --extensions-dir "$HOME_DIR/ext" \
  --extensionDevelopmentPath="$EXT" --extensionDevelopmentPath="$HERE/bootstrap" \
  ${ESCUREL_DEMO_CDP_PORT:+--remote-debugging-port=$ESCUREL_DEMO_CDP_PORT} ${ESCUREL_DEMO_CODE_ARGS:-} \
  --new-window "$HOME_DIR/workspace" > "$HOME_DIR/code.log" 2>&1 < /dev/null &
echo $! > "$HOME_DIR/code.pid"

echo "ready. gateway $(field gateway_url); story: $(cat "$HOME_DIR/story.json")"
echo "stop it with: $0 stop"
