# DuckLake validation spikes — results (ADR-0009 gate)

Scripts: [`spikes/ducklake/`](../../../spikes/ducklake/). Environment: DuckDB
CLI v1.5.4, extensions `ducklake d318a545` / `httpfs c3f215a` / `vss b833341`
/ `fts 6814ec9`; docker Postgres 16 + MinIO, plus the **real Cloud SQL +
GCS leg** (instance `ducklake-metadata`, bucket
`hetzner-agent-backplane-escurel-lake`, both europe-west1; dev machine on a
residential connection in Germany).

## Spike 1 — live Postgres catalog + S3 data path round-trip: PASS

- Writer (read-write ATTACH) and readers (`READ_ONLY` ATTACH) share one live
  Postgres catalog concurrently. No locks, no errors.
- **Atomicity/snapshot isolation holds**: a reader polling `count(*)` every
  0.3 s while the writer ran a 100-statement / 1 000-row transaction observed
  only `{1000, 2000}` — never a partial count. One transaction = exactly one
  DuckLake snapshot (`ducklake_snapshots()` id 2 → 3).
- **`FLOAT[768]` is NOT storable in DuckLake**: `CREATE TABLE … dense_vec
  FLOAT[768]` fails with *"Failed to convert DuckDB type to DuckLake —
  unsupported type FLOAT[768]"*. Lake columns must be `FLOAT[]` (list);
  the reader casts back `::FLOAT[768]` at bulk-load. Round-trip through
  Parquet preserves all 768 elements; HNSW + cosine query over the cast
  column works (self-match sanity check passes).
- **HNSW needs an in-memory DB on the reader**: `CREATE INDEX … USING HNSW`
  on a file-backed DB fails with *"HNSW indexes can only be created in
  in-memory databases, or when 'hnsw_enable_experimental_persistence' is
  set"*. Reader design: in-memory connection (+ `temp_directory` for spill);
  the experimental flag stays confined to the single-file backend.
- Timings (local docker; 2 000 rows): writer create+insert 1 000 rows 0.56 s;
  reader attach+fidelity checks+bulk-load+HNSW+query 2.9 s (includes
  extension loading); ATTACH + 10 `ducklake_snapshots()` polls 0.13 s —
  the change-detection poll is millisecond-cheap against a local catalog.
- **Cloud leg (real Cloud SQL + GCS): PASS**, same isolation result (poll saw
  only `{1000, 2000}`). The RTT tax: writer create+insert 1 000 rows 6.7 s
  (vs 0.56 s local); reader full adopt path 8.9 s (vs 2.9 s); per-op —
  ATTACH 0.76 s, `count(*)` over GCS Parquet 0.95 s, one
  `ducklake_snapshots()` poll **≈ 0.20 s**, bulk-load of 2 000 vector rows
  from GCS 1.36 s. Conclusion: a 30 s reader poll cadence costs ~0.7 % of a
  core; publish/adopt latencies are dominated by object-store round-trips,
  acceptable and expected to improve from substrate hosts (same region).

## Spike 2 — reader cold-start (bulk-load + in-memory HNSW + FTS): PASS

| blocks | load s | HNSW s | FTS s | peak RSS MB |
|---|---|---|---|---|
| 1 000 | 0.013 | 0.70 | 0.04 | 60 |
| 10 000 | 0.104 | 7.36 | 0.11 | 200 |
| 100 000 | 1.142 | 47.4 | 0.94 | 1 744 |

HNSW build dominates (~0.5 ms/block, `ef_construction=128, M=16`, in-memory).
Adopt happens off to the side while the old snapshot keeps serving, so even
the 100 k case (~50 s + transfer) only widens the eventual-consistency
window, not availability. Swap memory budget ≈ old + new RSS (≈ 2× table
above) — fine at current corpus sizes; document per-corpus budget before
pointing a 100 k-block tenant at a small container.

## Spike 3 — Phase B concurrent per-user appends via attached Postgres: PASS

Two DuckDB processes, each 250 single-statement INSERTs into the same table
through `ATTACH … (TYPE postgres)`: **500/500 rows, no lost writes**, 250
distinct seqs per writer, ~354 inserts/s aggregate (local docker PG; each
statement autocommits its own PG transaction). The plain-Postgres-tables
model for chat/events/CRDT re-homing is sound.

## Verdict

Proceed with the plan: Postgres-catalog DuckLake, `FLOAT[]` lake columns with
reader-side `::FLOAT[768]` cast, in-memory reader DBs, single writer /
`READ_ONLY` readers. Version pinning matters: the ducklake extension is
distributed per DuckDB version (CLI 1.5.4 here vs the crate's libduckdb
1.5.x) — `ESCUREL_DUCKLAKE_PIN` boot check stays in the plan.
