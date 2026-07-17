#!/usr/bin/env bash
# Spike 3 — Phase B model: per-user state (chat/events/CRDT) as plain
# Postgres tables written concurrently from many replicas through DuckDB's
# postgres extension (ATTACH … TYPE postgres, read-write INSERTs).
#
# Verifies: no lost writes under 2-way concurrent append; per-insert latency.
set -euo pipefail
source "$(dirname "$0")/_env.sh"

trap stop_all EXIT
start_pg

# schema via a bootstrap duckdb (no psql on this machine)
$DUCKDB -batch -init /dev/null <<SQL >/dev/null
INSTALL postgres; LOAD postgres;
ATTACH 'dbname=lake user=lake password=lake host=localhost port=$PG_PORT' AS userstate (TYPE postgres);
CREATE TABLE userstate.chat_like (writer INT, seq INT, user_id VARCHAR, body VARCHAR, ts TIMESTAMP DEFAULT now());
SQL

ROWS=250
append_loop() { # $1 = writer id — one INSERT per statement, own connection
  {
    echo "INSTALL postgres; LOAD postgres;"
    echo "ATTACH 'dbname=lake user=lake password=lake host=localhost port=$PG_PORT' AS userstate (TYPE postgres);"
    for i in $(seq $ROWS); do
      echo "INSERT INTO userstate.chat_like (writer, seq, user_id, body) VALUES ($1, $i, 'user-$1', 'msg $i');"
    done
  } | $DUCKDB -init /dev/null
}

t0=$(date +%s.%N)
append_loop 1 & p1=$!
append_loop 2 & p2=$!
wait $p1 $p2
t1=$(date +%s.%N)
elapsed=$(echo "$t1 - $t0" | bc)

$DUCKDB -batch -init /dev/null <<SQL
INSTALL postgres; LOAD postgres;
ATTACH 'dbname=lake user=lake password=lake host=localhost port=$PG_PORT' AS userstate (TYPE postgres, READ_ONLY);
SELECT 'total_rows (expect $((2 * ROWS)))', count(*) FROM userstate.chat_like;
SELECT 'per_writer', writer, count(*) FROM userstate.chat_like GROUP BY writer ORDER BY writer;
SELECT 'distinct_seq_per_writer', writer, count(DISTINCT seq) FROM userstate.chat_like GROUP BY writer ORDER BY writer;
SQL

echo "2×$ROWS concurrent appends in ${elapsed}s => $(echo "scale=1; 2 * $ROWS / $elapsed" | bc) inserts/s aggregate"
log "spike 03 done"
