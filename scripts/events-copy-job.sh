#!/bin/sh
# One-shot Postgres -> lake copy of the append surfaces (docs/deploy/events-to-lake.md).
#
# Copies the configured tenant's `escurel_events`, `escurel_drafts` and `escurel_chat_messages` from the
# attached Postgres tables (the lake catalog's database) into the lake, so that switching
# `ESCUREL_EVENTS_BACKEND` / `ESCUREL_CHAT_BACKEND` to `ducklake` does not orphan the existing rows. Exits 0
# ONLY when the copy finished and a second, independent dry run finds nothing left to copy and nothing that
# differs. Every other outcome (a writer holds the lease, the deadline passed, a destination row differs from
# its source, a verification mismatch, the CLI failed) exits non-zero, so a Kamal/k8s Job gates the swap on
# the real result.
#
# Steps:
#   1. DRY RUN: counts per table; the report is printed (and shows what an apply would do);
#   2. APPLY: takes the single-writer lease (refuses while a writer holds it: stop the writer first), copies,
#      verifies row counts and checksums per table, writes one `escurel:events-copy` audit event;
#   3. VERIFY: a second dry run must report `to_copy: 0` and `diverged: 0` for every table.
# The Postgres rows are only ever read. Rolling back is switching the environment back.
#
# Unlike migrate-kind-job there is NO throwaway server and so no auth, webhook or listener to defuse: the
# command works on the catalog database and the lake directly (it is the same stopped-writer command shape as
# `escurel-server lake-upgrade`).
#
# Env: ESCUREL_TENANT (required) and the writer's lake environment (ESCUREL_INDEX_BACKEND=ducklake,
#      ESCUREL_DUCKLAKE_CATALOG_DSN [a Postgres DSN], ESCUREL_DUCKLAKE_DATA_PATH, the object-store / gdrive
#      variables of the deployment), ESCUREL_JOB_DEADLINE_SECS (default 3600),
#      ESCUREL_EVENTS_COPY_TABLES (optional, default `events,drafts,chat`),
#      ESCUREL_SERVER_DATA_DIR (writable; the command creates it), ESCUREL_BIN_DIR (default: binaries on PATH).
set -eu

: "${ESCUREL_TENANT:?ESCUREL_TENANT is required}"
DEADLINE="${ESCUREL_JOB_DEADLINE_SECS:-3600}"
TABLES="${ESCUREL_EVENTS_COPY_TABLES:-events,drafts,chat}"
BIN="${ESCUREL_BIN_DIR:+$ESCUREL_BIN_DIR/}"
OUT="$(mktemp)"
START="$(date +%s)"

fail() {
    echo "events-copy-job: FAILED: $*" >&2
    rm -f "$OUT"
    exit 1
}
remaining() {
    echo $((DEADLINE - ($(date +%s) - START)))
}

case "$TABLES" in
    *[!a-z,]* | "" | ,* | *,)
        fail "ESCUREL_EVENTS_COPY_TABLES: \`$TABLES\`: expected a comma-separated list of events, drafts, chat"
        ;;
esac

[ "${ESCUREL_INDEX_BACKEND:-}" = "ducklake" ] || fail "ESCUREL_INDEX_BACKEND must be ducklake (the lake is the destination)"

# The command creates the data dir itself; a fresh k8s emptyDir has none and DuckDB needs $HOME.
DATA_DIR="${ESCUREL_SERVER_DATA_DIR:-/data}"
mkdir -p "$DATA_DIR" || fail "cannot create ESCUREL_SERVER_DATA_DIR=$DATA_DIR"
if [ -n "${HOME:-}" ]; then
    mkdir -p "$HOME" || fail "cannot create HOME=$HOME (DuckDB's home directory)"
fi

copy() {
    LEFT="$(remaining)"
    [ "$LEFT" -gt 0 ] || fail "deadline of ${DEADLINE}s used up before: $*"
    timeout "$LEFT" "${BIN}escurel-server" events-copy --tables "$TABLES" "$@" >"$OUT" ||
        fail "escurel-server events-copy $* failed or timed out (see above)"
    cat "$OUT"
}

# 1. Dry run.
copy

# 2. Apply.
copy --apply
grep -q '"applied": true' "$OUT" || fail "the apply did not report applied: true"

# 3. Verification: nothing left, nothing differing.
copy
if grep -Eq '"to_copy": [1-9]' "$OUT"; then
    fail "the verification dry run still finds rows to copy (see the report)"
fi
if grep -Eq '"diverged": [1-9]' "$OUT"; then
    fail "the verification dry run finds destination rows that differ from their source (see the report)"
fi

rm -f "$OUT"
echo "events-copy-job: OK" >&2
