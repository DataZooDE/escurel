# DuckDB `BLOB` columns round-trip byte-exact through an attached Postgres table

**Question.** DuckLake PR 10 (CRDT op-log re-homing) needs `crdt_ops.op_bytes`
and `crdt_snapshots.snapshot_bytes` to live in a Postgres table reached via
`ATTACH '<dsn>' AS crdt_pg (TYPE postgres)` (the same plain `TYPE postgres`
connector PRs 8/9 use for chat/events — NOT the `ducklake:postgres:` catalog
protocol). Postgres has no `BLOB` type; does DuckDB's Postgres connector
translate a `BLOB`-typed `CREATE TABLE` column to something that actually
stores arbitrary bytes without corruption (nul bytes, `0xFF`, non-UTF8)?

**Answer: yes, cleanly.** `CREATE TABLE ... (op_bytes BLOB NOT NULL)` issued
against an attached `(TYPE postgres)` alias is translated server-side to
`bytea`:

```
$ duckdb -c "
INSTALL postgres; LOAD postgres;
ATTACH 'host=127.0.0.1 port=55432 user=postgres password=pw dbname=verify' AS pgv (TYPE postgres);
CREATE TABLE IF NOT EXISTS pgv.crdt_ops_verify (
    page_id VARCHAR NOT NULL, op_id VARCHAR NOT NULL,
    op_bytes BLOB NOT NULL, PRIMARY KEY (page_id, op_id));"

$ docker exec pg psql -U postgres -d verify -c '\d crdt_ops_verify'
  Column  |       Type        | Nullable
----------+--------------------+----------
 page_id  | character varying | not null
 op_id    | character varying | not null
 op_bytes | bytea              | not null
```

A byte-exact round trip through `duckdb::ToSql` (the exact bind path
`DuckdbCrdtBackend::append_op` uses, not a SQL literal) confirms no
corruption for bytes including `0x00` and `0xFF`:

```rust
let bytes: Vec<u8> = vec![0x00, 0x01, 0xFE, 0xFF, 0x00, 0xAB, 0x10];
conn.execute("INSERT INTO pgv.rust_verify (id, b) VALUES (?, ?)",
             duckdb::params!["r1", bytes]).unwrap();
let got: Vec<u8> = conn.query_row("SELECT b FROM pgv.rust_verify WHERE id = ?",
             duckdb::params!["r1"], |row| row.get(0)).unwrap();
assert_eq!(got, bytes); // OK: [0, 1, 254, 255, 0, 171, 16] == [0, 1, 254, 255, 0, 171, 16]
```

Verified against a `postgres:17` Docker container and the workspace's pinned
`libduckdb-sys` (downloaded release, see `.cargo/config.toml`), 2026-07-18.

**Gotcha found along the way (not the main question, but a trap for the next
person poking at this with the CLI).** DuckDB's BLOB literal syntax needs a
`\x` escape **per byte** — `'\x00010203FEFF00'::BLOB` is NOT "one hex string",
it parses as the single byte `0x00` followed by the *literal ASCII text*
`"010203FEFF00"` (`octet_length` 13, not 7). The correct literal is
`'\x00\x01\x02\x03\xFE\xFF\x00'::BLOB`. This bit only the ad-hoc CLI probe,
not the actual `duckdb::ToSql` bind path production code uses — parameter
binding was correct on the first try.

**Conclusion for PR 10.** `crdt_pg.crdt_ops` / `crdt_pg.crdt_snapshots` can
declare `op_bytes BLOB` / `snapshot_bytes BLOB` in the `CREATE TABLE` DDL
exactly as the local schema does — no `FLOAT[768]`-style substitution needed
(unlike PR 8's `dense_vec`, which DID need `FLOAT[]` instead of `FLOAT[768]`
because DuckLake/Postgres attaches reject fixed-width array columns). `BLOB`
was already the right type; this note exists to record that it was verified,
not assumed.

**How to recognise it next time.** Any DuckDB column typed `BLOB` on a table
reached through `ATTACH ... (TYPE postgres)` maps to Postgres `bytea` and
round-trips exactly via `duckdb::ToSql`/`row.get::<_, Vec<u8>>`. If you see
corruption on such a column, look elsewhere first (e.g. an accidental
`::VARCHAR` cast somewhere in the query) — the BLOB↔bytea boundary itself is
not the culprit.
