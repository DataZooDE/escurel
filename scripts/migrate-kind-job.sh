#!/bin/sh
# One-shot `type:` -> `kind:` migration job (docs/deploy/kind-migration.md §3).
#
# Boots a throwaway escurel-server on the container's loopback over the mounted data volume, runs
# `escurel admin migrate-kind --apply`, and exits 0 ONLY when the migration finished and the tenant
# is no longer quarantined. Every other outcome (server died at boot, deadline passed, a conflict
# left the tenant quarantined, the CLI failed) exits non-zero, so a Kamal/k8s Job gates the swap on
# the real result instead of on "the shell reached its last line".
#
# Env: ESCUREL_TENANT (required), the SAME embedder env as production (the apply rebuilds the index
#      with it), ESCUREL_JOB_DEADLINE_SECS (default 3600: boot + apply together),
#      ESCUREL_JOB_PORT (default 8080), ESCUREL_BIN_DIR (default: binaries on PATH).
set -eu

: "${ESCUREL_TENANT:?ESCUREL_TENANT is required}"
DEADLINE="${ESCUREL_JOB_DEADLINE_SECS:-3600}"
PORT="${ESCUREL_JOB_PORT:-8080}"
BIN="${ESCUREL_BIN_DIR:+$ESCUREL_BIN_DIR/}"
URL="http://127.0.0.1:$PORT"
OUT="$(mktemp)"
START="$(date +%s)"
SERVER_PID=""

stop_server() {
    if [ -n "$SERVER_PID" ] && kill -0 "$SERVER_PID" 2>/dev/null; then
        kill -TERM "$SERVER_PID" 2>/dev/null || true
        wait "$SERVER_PID" 2>/dev/null || true
    fi
}
fail() {
    echo "migrate-kind-job: FAILED: $*" >&2
    stop_server
    rm -f "$OUT"
    exit 1
}
remaining() {
    echo $((DEADLINE - ($(date +%s) - START)))
}

ESCUREL_SERVER_LISTEN_HTTP="127.0.0.1:$PORT" ESCUREL_OBSERVABILITY_METRICS_LISTEN= \
    "${BIN}escurel-server" &
SERVER_PID=$!

# Wait for the listener, but notice a server that died at boot instead of looping forever.
until curl -fsS "$URL/healthz" >/dev/null 2>&1; do
    kill -0 "$SERVER_PID" 2>/dev/null || fail "escurel-server exited during boot"
    [ "$(remaining)" -gt 0 ] || fail "escurel-server did not listen within ${DEADLINE}s"
    sleep 1
done

LEFT="$(remaining)"
[ "$LEFT" -gt 0 ] || fail "deadline of ${DEADLINE}s used up before the apply started"
"${BIN}escurel" --server "$URL" admin migrate-kind --tenant "$ESCUREL_TENANT" --apply \
    --timeout-secs "$LEFT" >"$OUT" || fail "migrate-kind --apply failed (see above)"
cat "$OUT"

grep -q '"tenant_quarantined": false' "$OUT" ||
    fail "the migration finished but the tenant is still quarantined (conflicts / signed pack pages: see the report above)"

kill -0 "$SERVER_PID" 2>/dev/null || fail "escurel-server died during the migration"
kill -TERM "$SERVER_PID"
wait "$SERVER_PID" || fail "escurel-server did not exit cleanly on SIGTERM"
SERVER_PID=""
rm -f "$OUT"
echo "migrate-kind-job: OK" >&2
