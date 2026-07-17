#!/usr/bin/env bash
# Spike 1 — DuckLake with a live Postgres catalog + S3-compatible data path.
#
# Questions this answers (plan §Validation spikes):
#   a) Can a writer (read-write attach) and readers (READ_ONLY attach) share
#      one live Postgres catalog concurrently?
#   b) Is a multi-statement writer transaction atomic from a polling reader's
#      point of view (snapshot isolation — reader never sees a partial batch)?
#   c) Does FLOAT[768] survive the Parquet round-trip, or does it widen
#      (=> adopt path needs a cast before the HNSW build)?
#   d) ducklake_snapshots() as the reader's change-detection poll.
#   e) Wall-clock: attach, catalog ops, bulk-load to local table, HNSW build,
#      one cosine query — the adopt-path budget.
#
# Local leg: docker Postgres + MinIO (always runs).
# Cloud leg: set ESCUREL_SPIKE_PG_DSN (Cloud SQL DSN) + ESCUREL_SPIKE_DATA_PATH
#            (gs://…) + ESCUREL_SPIKE_GCS_KEY_ID/_SECRET to also run against
#            the real backplane.
set -euo pipefail
source "$(dirname "$0")/_env.sh"

run_leg() { # $1 leg-name  $2 pg-dsn  $3 data-path  $4 secret-sql
  local leg=$1 dsn=$2 data_path=$3 secret_sql=$4
  local out="$WORK/$leg" t0 t1
  mkdir -p "$out"
  log "[$leg] catalog: ${dsn%%password=*}…  data: $data_path"

  # -- writer: create + first batch ----------------------------------------
  t0=$(date +%s.%N)
  $DUCKDB -batch -init /dev/null "$out/writer.duckdb" <<SQL
$SQL_EXT_LAKE
$secret_sql
ATTACH 'ducklake:postgres:$dsn' AS lake (DATA_PATH '$data_path');
SELECT 'ducklake_version', extension_version FROM duckdb_extensions() WHERE extension_name='ducklake';
CREATE OR REPLACE TABLE lake.blocks_like (block_id BIGINT, page_id VARCHAR, body VARCHAR, dense_vec FLOAT[]);
BEGIN;
INSERT INTO lake.blocks_like SELECT i, 'page-'||i, 'body '||i, $SQL_RANDVEC_LIST FROM range(1000) t(i);
COMMIT;
SELECT 'writer_count', count(*) FROM lake.blocks_like;
SELECT 'snapshot_after_batch1', max(snapshot_id) FROM ducklake_snapshots('lake');
SQL
  t1=$(date +%s.%N)
  echo "[$leg] writer create+insert 1000 rows: $(echo "$t1 - $t0" | bc)s"

  # -- atomicity: reader polls while writer runs one long multi-stmt tx ----
  ( # writer: 100 INSERT statements inside ONE transaction (second batch)
    sleep 1
    {
      echo "$SQL_EXT_LAKE $secret_sql"
      echo "ATTACH 'ducklake:postgres:$dsn' AS lake (DATA_PATH '$data_path');"
      echo "BEGIN;"
      for _ in $(seq 100); do
        echo "INSERT INTO lake.blocks_like SELECT 1000+i, 'p', 'b', $SQL_RANDVEC_LIST FROM range(10) t(i);"
      done
      echo "COMMIT;"
    } | $DUCKDB -init /dev/null "$out/writer2.duckdb"
  ) &
  local writer_pid=$!
  : > "$out/poll.txt"
  poll_once() {
    $DUCKDB -csv -batch -init /dev/null <<SQL >> "$out/poll.txt" 2>/dev/null || true
$SQL_EXT_LAKE
$secret_sql
ATTACH 'ducklake:postgres:$dsn' AS lake (DATA_PATH '$data_path', READ_ONLY);
SELECT count(*) FROM lake.blocks_like;
SQL
  }
  while kill -0 $writer_pid 2>/dev/null; do
    poll_once
    sleep 0.3
  done
  wait $writer_pid
  poll_once   # capture the post-commit state too
  local counts
  counts=$(grep -E '^[0-9]+$' "$out/poll.txt" | sort -un | tr '\n' ' ')
  echo "[$leg] poll-observed row counts during 1000-row tx (expect only 1000 and 2000): $counts"

  # -- reader: fidelity + change detection + adopt-path timing -------------
  # NB: in-memory DB. Spike finding: a file-backed DB refuses CREATE INDEX
  # … USING HNSW without hnsw_enable_experimental_persistence; the reader
  # design is therefore an in-memory connection (never persisted/reloaded).
  t0=$(date +%s.%N)
  $DUCKDB -batch -init /dev/null <<SQL
$SQL_EXT_LAKE
INSTALL vss; LOAD vss;
$secret_sql
.timer on
ATTACH 'ducklake:postgres:$dsn' AS lake (DATA_PATH '$data_path', READ_ONLY);
SELECT 'reader_count', count(*) FROM lake.blocks_like;
SELECT 'typeof_dense_vec', typeof(dense_vec) FROM lake.blocks_like LIMIT 1;
SELECT 'max_snapshot', max(snapshot_id) FROM ducklake_snapshots('lake');
CREATE TABLE local_blocks AS SELECT block_id, page_id, body, dense_vec::FLOAT[768] AS dense_vec FROM lake.blocks_like;
DETACH lake;
CREATE INDEX hnsw_local ON local_blocks USING HNSW (dense_vec) WITH (metric='cosine', ef_construction=128, ef_search=64, M=16);
SELECT 'cosine_top1', block_id FROM local_blocks ORDER BY array_cosine_distance(dense_vec, (SELECT dense_vec FROM local_blocks WHERE block_id=42)) LIMIT 1;
SQL
  t1=$(date +%s.%N)
  echo "[$leg] reader attach+fidelity+bulk-load+HNSW+query total: $(echo "$t1 - $t0" | bc)s"

  # -- catalog-op latency (the RTT tax): 10 snapshot polls -----------------
  t0=$(date +%s.%N)
  $DUCKDB -batch -init /dev/null <<SQL >/dev/null
$SQL_EXT_LAKE
$secret_sql
ATTACH 'ducklake:postgres:$dsn' AS lake (DATA_PATH '$data_path', READ_ONLY);
$(for _ in $(seq 10); do echo "SELECT max(snapshot_id) FROM ducklake_snapshots('lake');"; done)
SQL
  t1=$(date +%s.%N)
  echo "[$leg] attach + 10 snapshot polls: $(echo "$t1 - $t0" | bc)s"
}

# ---- local leg -------------------------------------------------------------
trap stop_all EXIT
start_pg
start_minio
run_leg local "$PG_DSN_LOCAL" "s3://$MINIO_BUCKET/data/" "$SQL_SECRET_MINIO"

# ---- cloud leg (optional) ---------------------------------------------------
if [[ -n "${ESCUREL_SPIKE_PG_DSN:-}" ]]; then
  SQL_SECRET_GCS="CREATE OR REPLACE SECRET spike_store (TYPE gcs, KEY_ID '${ESCUREL_SPIKE_GCS_KEY_ID}', SECRET '${ESCUREL_SPIKE_GCS_SECRET}');"
  run_leg cloud "$ESCUREL_SPIKE_PG_DSN" "$ESCUREL_SPIKE_DATA_PATH" "$SQL_SECRET_GCS"
else
  log "cloud leg skipped (ESCUREL_SPIKE_PG_DSN unset)"
fi

log "spike 01 done; artifacts in $WORK"
