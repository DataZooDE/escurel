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
#   1. a `migrate-kind` DRY RUN that must report no conflicts (the server boot adopts a lake that is one
#      declared-compatible schema version behind AS-IS: nothing needs re-pinning first);
#   2. `migrate-kind --apply`, which publishes the migrated corpus to the lake itself (the report
#      carries `lake_published`; its absence fails the job);
#   3. a SECOND boot over the lake, which must come up NOT quarantined (a stale lake that still holds
#      `type:` pages quarantines the tenant, so this is the "no legacy page left in the lake" check)
#      and whose dry run must find nothing left to migrate;
#   4. LAST, `escurel-server lake-upgrade`: pins the manifest to this build's schema version when the
#      publish did not (idempotent).
# The one-way step comes LAST on purpose. Re-pinning the lake (and any publish does it) makes the PREVIOUS
# release refuse the lake (`lake schema_version 10 != reader schema_version 9`), so a job that failed after
# that point stranded the rollback target (the lab cutover of 2026-10-15, attempt 2). Everything that can fail
# on configuration, auth or a conflict now fails BEFORE the lake is touched. If a cutover still has to be
# walked back after the publish, `escurel-server lake-upgrade --to <previous>` restores the pin (a compatible
# bump only; the migrated lane content comes back from the backup).
# The job NEVER uses `escurel admin rebuild` (its client deadline and a partial index were how the
# 2026-10-14 rehearsal almost published 91 of 183 pages).
#
# Threat model: the throwaway server binds 127.0.0.1 only, inside a Job pod with no Service and no ingress,
# and runs WITHOUT auth: the job's own CLI is its only caller. So the job drops every ESCUREL_AUTH_* variable
# the Deployment's environment carries (the lab sets the OIDC issuer; an inherited issuer made the job's own
# unauthenticated calls fail with `http 401`), plus the other knobs of the real gateway that a throwaway
# server must not act on: the outbound webhook, the periodic lake publish (it would re-pin the manifest on
# its own timer) and the metrics listener.
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

# A k8s Job starts on a FRESH emptyDir: nothing has created the data dir yet, and DuckDB refuses to open at
# all when $HOME (the pods point it at the data dir) is missing ("Can't find the home directory at ...",
# the lab cutover of 2026-10-15). The binaries create both themselves; this is belt and braces.
DATA_DIR="${ESCUREL_SERVER_DATA_DIR:-/data}"
mkdir -p "$DATA_DIR" || fail "cannot create ESCUREL_SERVER_DATA_DIR=$DATA_DIR"
if [ -n "${HOME:-}" ]; then
    mkdir -p "$HOME" || fail "cannot create HOME=$HOME (DuckDB's home directory)"
fi

LAKE=0
[ "${ESCUREL_INDEX_BACKEND:-}" = "ducklake" ] && LAKE=1

# The Deployment's gateway config is not this job's config (see "Threat model" above). `unset` every auth
# variable, indexed ones (ESCUREL_AUTH_OIDC_ISSUER_2, ...) included.
for v in $(env | sed -n 's/^\(ESCUREL_AUTH_[A-Za-z0-9_]*\)=.*/\1/p'); do
    unset "$v"
done
unset ESCUREL_WEBHOOK_URL ESCUREL_WEBHOOK_SECRET

start_server() {
    ESCUREL_SERVER_LISTEN_HTTP="127.0.0.1:$PORT" ESCUREL_OBSERVABILITY_METRICS_LISTEN= \
        ESCUREL_SNAPSHOT_PUBLISH_SECS=0 \
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

start_server

# 1. Dry run: conflicts block the apply (it would leave the tenant quarantined).
LEFT="$(remaining)"
[ "$LEFT" -gt 0 ] || fail "deadline of ${DEADLINE}s used up before the dry run started"
"${BIN}escurel" --server "$URL" admin migrate-kind --tenant "$ESCUREL_TENANT" \
    --timeout-secs "$LEFT" >"$OUT" || fail "migrate-kind dry run failed (see above)"
cat "$OUT"
grep -q '"conflicts": \[\]' "$OUT" ||
    fail "the dry run reports conflicts (a page with both keys): resolve them before applying"

# 2. Apply.
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

# 3. DuckLake: boot AGAIN over the lake, exactly as production will. A lake that still holds `type:`
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

    # 4. The one-way step, LAST: pin the manifest to this build's version (a no-op when the publish already did).
    "${BIN}escurel-server" lake-upgrade >"$OUT" || fail "lake-upgrade failed (see above)"
    cat "$OUT"
fi

rm -f "$OUT"
echo "migrate-kind-job: OK" >&2
