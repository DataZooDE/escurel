#!/usr/bin/env bash
# Shared setup for the DuckLake validation spikes (ADR-0009 / plan
# tranquil-orbiting-frost). Local legs use docker Postgres + MinIO;
# the Cloud SQL + GCS leg of spike 01 activates when ESCUREL_SPIKE_PG_DSN
# and ESCUREL_SPIKE_GCS_* are exported (see the runbook in the plan).
#
# Not product code: no error-handling polish, measurable output only.
set -euo pipefail

SPIKE_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
WORK="${SPIKE_WORK:-$(mktemp -d /tmp/ducklake-spike.XXXXXX)}"

PG_PORT=55432
PG_CONTAINER=ducklake-spike-pg
MINIO_PORT=59000
MINIO_CONTAINER=ducklake-spike-minio
MINIO_KEY=minioadmin
MINIO_SECRET=minioadmin
MINIO_BUCKET=lake

DUCKDB=${DUCKDB:-duckdb}

log() { printf '\n== %s\n' "$*" >&2; }

start_pg() {
  docker rm -f "$PG_CONTAINER" >/dev/null 2>&1 || true
  docker run -d --name "$PG_CONTAINER" -p "$PG_PORT:5432" \
    -e POSTGRES_USER=lake -e POSTGRES_PASSWORD=lake -e POSTGRES_DB=lake \
    postgres:16-alpine >/dev/null
  for _ in $(seq 60); do
    docker exec "$PG_CONTAINER" pg_isready -U lake -d lake >/dev/null 2>&1 && return 0
    sleep 0.5
  done
  echo "postgres did not become ready" >&2; exit 1
}

start_minio() {
  docker rm -f "$MINIO_CONTAINER" >/dev/null 2>&1 || true
  docker run -d --name "$MINIO_CONTAINER" -p "$MINIO_PORT:9000" \
    -e MINIO_ROOT_USER=$MINIO_KEY -e MINIO_ROOT_PASSWORD=$MINIO_SECRET \
    minio/minio server /data >/dev/null
  for _ in $(seq 60); do
    curl -sf "http://localhost:$MINIO_PORT/minio/health/ready" >/dev/null 2>&1 && break
    sleep 0.5
  done
  docker run --rm --network host --entrypoint sh minio/mc -c \
    "mc alias set m http://localhost:$MINIO_PORT $MINIO_KEY $MINIO_SECRET >/dev/null && mc mb -p m/$MINIO_BUCKET >/dev/null"
}

stop_all() {
  docker rm -f "$PG_CONTAINER" "$MINIO_CONTAINER" >/dev/null 2>&1 || true
}

PG_DSN_LOCAL="host=localhost port=$PG_PORT dbname=lake user=lake password=lake"

# SQL fragments -------------------------------------------------------------

# duckdb CLI ships autoinstall; be explicit anyway (known gotcha: bare LOAD
# does not autoinstall — docs/notes/discovered/2026-05-24-duckdb-load-needs-install.md).
SQL_EXT_LAKE="INSTALL ducklake; LOAD ducklake; INSTALL postgres; LOAD postgres; INSTALL httpfs; LOAD httpfs;"

SQL_SECRET_MINIO="CREATE OR REPLACE SECRET spike_store (TYPE s3, KEY_ID '$MINIO_KEY', SECRET '$MINIO_SECRET', ENDPOINT 'localhost:$MINIO_PORT', URL_STYLE 'path', USE_SSL false, REGION 'us-east-1');"

# 768-dim random vector as FLOAT[768]
SQL_RANDVEC="list_transform(range(768), x -> random()::FLOAT)::FLOAT[768]"
# …and as FLOAT[] (list): DuckLake rejects fixed-size ARRAY columns
# ("unsupported type FLOAT[768]"), so lake tables store lists and the
# reader casts back to FLOAT[768] before building HNSW.
SQL_RANDVEC_LIST="list_transform(range(768), x -> random()::FLOAT)"
