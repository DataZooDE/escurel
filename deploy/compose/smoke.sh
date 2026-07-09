#!/usr/bin/env bash
# Smoke test for the generic Compose deploy: build, up, /healthz, restart,
# /healthz again. Exit 0 = pass. Requires Docker + the Compose plugin.
#
# Isolated from any real .env: uses a throwaway project name + env file so it
# never touches an operator's running stack.
set -euo pipefail
cd "$(dirname "$0")"

PROJECT="escurel-smoke-$$"
ENVFILE=".env.smoke.$$"
PORT="${ESCUREL_SMOKE_PORT:-18080}"

# Only the host port mapping needs interpolation vars; the container itself
# runs on the image defaults (always-rebuild, keyless-zero embedder) — exactly
# the container path we want to smoke-test.
cat > "$ENVFILE" <<EOF
ESCUREL_HTTP_PORT=${PORT}
ESCUREL_METRICS_PORT=$((PORT + 1))
EOF

compose() { docker compose -p "$PROJECT" --env-file "$ENVFILE" "$@"; }

cleanup() {
  compose down -v >/dev/null 2>&1 || true
  rm -f "$ENVFILE"
}
trap cleanup EXIT

wait_healthz() {
  for _ in $(seq 1 60); do
    if curl -fsS "http://127.0.0.1:${PORT}/healthz" >/dev/null 2>&1; then
      return 0
    fi
    sleep 2
  done
  echo "FAIL: /healthz never came up" >&2
  compose logs escurel >&2 || true
  return 1
}

echo "== build + up =="
compose up -d --build

echo "== wait for /healthz =="
wait_healthz
body="$(curl -fsS "http://127.0.0.1:${PORT}/healthz")"
[ "$body" = "OK" ] || { echo "FAIL: /healthz body='$body'" >&2; exit 1; }

echo "== restart (STOP-FIRST) and re-check =="
compose restart escurel
wait_healthz

echo "PASS: compose stack healthy across a restart"
