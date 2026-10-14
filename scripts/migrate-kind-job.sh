#!/bin/sh
# One-shot `type:` -> `kind:` migration job (docs/deploy/kind-migration.md §3).
#
# Boots a throwaway escurel-server on the container's loopback over the mounted data volume, runs
# `escurel admin migrate-kind --apply`, and exits 0 ONLY when the migration finished and the tenant
# is no longer quarantined. Every other outcome (server died at boot, deadline passed, a conflict
# left the tenant quarantined, the CLI failed) exits non-zero, so a Kamal/k8s Job gates the swap on
# the real result instead of on "the shell reached its last line".
#
# On a DuckLake deployment (ESCUREL_INDEX_BACKEND=ducklake, the ESCUREL_DUCKLAKE_* env of the writer)
# the job also keeps the LAKE consistent, because a writer adopts the lake over the lane at boot:
#   1. `escurel-server lake-upgrade` (before any server boots: re-pins a lake one declared-compatible
#      schema version behind; refuses an unknown jump or a newer lake);
#   2. a `migrate-kind` DRY RUN that must report no conflicts;
#   3. `migrate-kind --apply`, which publishes the migrated corpus to the lake itself (the report
#      carries `lake_published`; its absence fails the job);
#   4. a SECOND boot over the lake, which must come up NOT quarantined (a stale lake that still holds
#      `type:` pages quarantines the tenant, so this is the "no legacy page left in the lake" check)
#      and whose dry run must find nothing left to migrate.
# The job NEVER uses `escurel admin rebuild` (its client deadline and a partial index were how the
# 2026-10-14 rehearsal almost published 91 of 183 pages).
#
# Plan the Job deadline for >= 3600 s: the rehearsal on 183 pages / 988 snapshots took ~15 min
# (boot 36 s, dry run 295 s, apply 831 s) and a second boot adds ~35 s.
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

LAKE=0
[ "${ESCUREL_INDEX_BACKEND:-}" = "ducklake" ] && LAKE=1

start_server() {
    ESCUREL_SERVER_LISTEN_HTTP="127.0.0.1:$PORT" ESCUREL_OBSERVABILITY_METRICS_LISTEN= \
        "${BIN}escurel-server" &
    SERVER_PID=$!
    # Wait for the listener, but notice a server that died at boot instead of looping forever.
    until curl -fsS "$URL/healthz" >/dev/null 2>&1; do
        kill -0 "$SERVER_PID" 2>/dev/null || fail "escurel-server exited during boot"
        [ "$(remaining)" -gt 0 ] || fail "escurel-server did not listen within ${DEADLINE}s"
        sleep 1
    done
}

stop_server_clean() {
    kill -0 "$SERVER_PID" 2>/dev/null || fail "escurel-server died during the migration"
    kill -TERM "$SERVER_PID"
    wait "$SERVER_PID" || fail "escurel-server did not exit cleanly on SIGTERM"
    SERVER_PID=""
}

# 1. The lake schema pin (DuckLake only), before any server touches the lake.
if [ "$LAKE" = 1 ]; then
    "${BIN}escurel-server" lake-upgrade >"$OUT" || fail "lake-upgrade failed (see above)"
    cat "$OUT"
fi

start_server

# 2. Dry run: conflicts block the apply (it would leave the tenant quarantined).
LEFT="$(remaining)"
[ "$LEFT" -gt 0 ] || fail "deadline of ${DEADLINE}s used up before the dry run started"
"${BIN}escurel" --server "$URL" admin migrate-kind --tenant "$ESCUREL_TENANT" \
    --timeout-secs "$LEFT" >"$OUT" || fail "migrate-kind dry run failed (see above)"
cat "$OUT"
grep -q '"conflicts": \[\]' "$OUT" ||
    fail "the dry run reports conflicts (a page with both keys): resolve them before applying"

# 3. Apply.
LEFT="$(remaining)"
[ "$LEFT" -gt 0 ] || fail "deadline of ${DEADLINE}s used up before the apply started"
"${BIN}escurel" --server "$URL" admin migrate-kind --tenant "$ESCUREL_TENANT" --apply \
    --timeout-secs "$LEFT" >"$OUT" || fail "migrate-kind --apply failed (see above)"
cat "$OUT"

grep -q '"tenant_quarantined": false' "$OUT" ||
    fail "the migration finished but the tenant is still quarantined (conflicts / signed pack pages: see the report above)"
if [ "$LAKE" = 1 ]; then
    grep -q '"lake_published": {' "$OUT" ||
        fail "the migration finished but the lake was not published (no lake_published in the report)"
fi

stop_server_clean

# 4. DuckLake: boot AGAIN over the lake, exactly as production will. A lake that still holds `type:`
#    pages quarantines the tenant at this boot, and a clean second dry run proves nothing is left.
if [ "$LAKE" = 1 ]; then
    start_server
    HDRS="$(curl -sS -D - -o /dev/null "$URL/readyz")" || fail "/readyz unreachable on the verification boot"
    if printf '%s' "$HDRS" | grep -qi '^x-escurel-quarantined'; then
        fail "the verification boot is QUARANTINED: the lake still holds legacy \`type:\` pages"
    fi
    LEFT="$(remaining)"
    [ "$LEFT" -gt 0 ] || fail "deadline of ${DEADLINE}s used up before the verification dry run"
    "${BIN}escurel" --server "$URL" admin migrate-kind --tenant "$ESCUREL_TENANT" \
        --timeout-secs "$LEFT" >"$OUT" || fail "verification dry run failed (see above)"
    grep -q '"pages_to_migrate": \[\]' "$OUT" ||
        fail "the verification dry run still finds pages to migrate (see the report)"
    stop_server_clean
fi

rm -f "$OUT"
echo "migrate-kind-job: OK" >&2
