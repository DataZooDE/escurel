#!/usr/bin/env bash
# Spike 2 — reader cold-start budget: bulk-load + in-memory HNSW build + FTS
# rebuild time and peak RSS at N ∈ {1k, 10k, 100k} blocks × FLOAT[768].
#
# This sizes ESCUREL_SNAPSHOT_REFRESH_SECS and the ×2 memory window during
# the reader's Indexer swap. Pure local duckdb — no containers.
set -euo pipefail
source "$(dirname "$0")/_env.sh"

printf '%-8s %-12s %-12s %-12s %-10s\n' N load_s hnsw_s fts_s peak_rss_mb

for N in 1000 10000 100000; do
  src="$WORK/src_$N.duckdb"
  dst="$WORK/dst_$N.duckdb"
  rm -f "$src" "$dst"

  # source table = "the attached lake" stand-in
  $DUCKDB -batch -init /dev/null "$src" <<SQL >/dev/null
CREATE TABLE blocks_like AS
  SELECT i AS block_id, 'page-'||(i//10) AS page_id,
         'lorem ipsum dolor sit amet block '||i||' körper text nummer '||i AS body,
         $SQL_RANDVEC AS dense_vec
  FROM range($N) t(i);
SQL

  # measured phase: attach + bulk load + HNSW + FTS. In-memory DB (no file
  # arg): file-backed DBs refuse HNSW creation without the experimental-
  # persistence flag — matches the reader design. Peak RSS sampled from
  # /proc (no GNU time on this box).
  $DUCKDB -batch -init /dev/null <<SQL > "$WORK/out_$N.txt" 2>&1 &
INSTALL vss; LOAD vss; INSTALL fts; LOAD fts;
ATTACH '$src' AS snap (READ_ONLY);
.timer on
CREATE TABLE blocks AS SELECT * FROM snap.blocks_like;
CREATE INDEX hnsw_blocks ON blocks USING HNSW (dense_vec) WITH (metric='cosine', ef_construction=128, ef_search=64, M=16);
PRAGMA create_fts_index('blocks', 'block_id', 'body', stemmer='german', overwrite=1);
SQL
  pid=$!
  rss_kb=0
  while kill -0 $pid 2>/dev/null; do
    cur=$(grep VmHWM "/proc/$pid/status" 2>/dev/null | grep -oE '[0-9]+' || echo 0)
    (( cur > rss_kb )) && rss_kb=$cur
    sleep 0.2
  done
  wait $pid

  # duckdb .timer prints "Run Time (s): real X.XXX ..." per statement; grab the 3 measured ones
  mapfile -t times < <(grep -oE 'real [0-9.]+' "$WORK/out_$N.txt" | awk '{print $2}' | tail -3)
  printf '%-8s %-12s %-12s %-12s %-10s\n' "$N" "${times[0]:-?}" "${times[1]:-?}" "${times[2]:-?}" "$((rss_kb / 1024))"
done

log "spike 02 done; artifacts in $WORK"
