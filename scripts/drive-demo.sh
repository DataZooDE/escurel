#!/usr/bin/env bash
# Behavioral browser verification of the capability demo via Flutter's
# own integration_test harness (`flutter drive` in Chrome through
# chromedriver). Unlike a DOM automation tool, tester.tap/enterText go
# through Flutter's gesture system, so widget callbacks actually fire;
# each test then re-reads the real escurel-server to prove the UI
# action reached the backend.
#
# Pairs with scripts/verify-demo.sh (rodney: presence + screenshots).
#
# Requires: chromedriver + Chrome on PATH, flutter, cargo.
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
APP="$ROOT/apps/escurel-explore"
PORT="${ESCUREL_DEMO_PORT:-8080}"
CHROMEDRIVER_PORT="${CHROMEDRIVER_PORT:-4444}"
BASE="http://127.0.0.1:$PORT"

fail() { echo "FAIL: $*" >&2; exit 1; }
note() { echo ">>> $*"; }

command -v chromedriver >/dev/null || fail "chromedriver not on PATH"
command -v flutter >/dev/null || fail "flutter not on PATH"

# 1. escurel-server in dev mode. demo_dir is set so the gateway also
#    sends permissive CORS (the flutter-drive web-server is a separate
#    origin). ZeroEmbedder; no OIDC → admin tools open.
DATA_DIR="$(mktemp -d)"
note "starting escurel-server (data=$DATA_DIR)"
ESCUREL_SERVE_DEMO_DIR="$APP/build/web" \
ESCUREL_SERVER_DATA_DIR="$DATA_DIR" \
ESCUREL_SERVER_LISTEN_HTTP="127.0.0.1:$PORT" \
ESCUREL_EMBEDDING_PROVIDER="zero" \
  cargo run -q -p escurel-server >"$ROOT/target/escurel-drive.log" 2>&1 &
SERVER_PID=$!

note "starting chromedriver on :$CHROMEDRIVER_PORT"
chromedriver --port="$CHROMEDRIVER_PORT" >"$ROOT/target/chromedriver.log" 2>&1 &
DRIVER_PID=$!

cleanup() {
  kill "$SERVER_PID" "$DRIVER_PID" >/dev/null 2>&1 || true
  rm -rf "$DATA_DIR"
}
trap cleanup EXIT

note "waiting for /healthz"
for _ in $(seq 1 90); do curl -fsS "$BASE/healthz" >/dev/null 2>&1 && break; sleep 1; done
curl -fsS "$BASE/healthz" >/dev/null 2>&1 || fail "server did not come up; see target/escurel-drive.log"

# 2. Drive the demo in Chrome. The app talks to the real gateway via
#    the dart-define'd base URL (HTTP-MCP transport).
note "flutter drive (this compiles the app for web; first run is slow)"
( cd "$APP" && flutter drive \
    --driver=test_driver/integration_test.dart \
    --target=integration_test/demo_test.dart \
    -d web-server \
    --browser-name=chrome \
    --driver-port="$CHROMEDRIVER_PORT" \
    --headless \
    --dart-define=ESCUREL_EXPLORE_MODE=http \
    --dart-define=ESCUREL_EXPLORE_BASE_URL="$BASE" ) || fail "flutter drive reported failures"

note "ALL INTEGRATION TESTS PASSED"
echo "PASS"
