# SQL-view re-materialisation on a persistent connection: alias collision + stale catalog

**Symptom.** Surfaced only once a `sql_view` was exercised over a **live DB
connector** (a real Postgres testcontainer — the offline `json_dir`/`parquet_dir`
tests can't reach this path). Two distinct bugs, both in
`materialise_view_on` / `prepare_source` (`escurel-index/src/backend/sql_view.rs`):

1. **Alias collision → healthy binding falsely `backend_unavailable`.**
   `validate_bindings` (and `reconstruct_views`) re-`ATTACH … AS <alias>` on the
   Indexer's **persistent** DuckDB connection, where the alias was *already*
   attached at `create_instance`. A plain `ATTACH` fails with
   `Binder Error: database with name "<alias>" already exists`, so
   `validate_bindings` reported a perfectly healthy view as
   `backend_unavailable` — a fail-closed false-positive that breaks legitimate
   reads (violates REQ-NF-06's intent).

2. **Stale catalog → source drift missed.** The postgres/mysql scanners cache
   the remote catalog at ATTACH time. After fixing (1) with
   `ATTACH IF NOT EXISTS`, a re-probe reused the cached catalog, so
   `CREATE OR REPLACE VIEW … SELECT *` re-expanded against the *old* columns and
   the schema fingerprint never changed — an `ALTER TABLE … ADD COLUMN` on the
   live source went undetected, so `validate_bindings` returned `ok` instead of
   `binding_degraded`.

**Fix.** In `prepare_source`'s DB-connector arm:
- `ATTACH IF NOT EXISTS '<secret>' AS <alias> (TYPE …, READ_ONLY)` — idempotent,
  so a re-probe on the live connection never collides. (Do **not** `DETACH`
  first: another managed view may share the same alias.)
- Then clear the scanner's cached catalog so `SELECT *` sees the live schema:
  `CALL pg_clear_cache()` for postgres, `CALL mysql_clear_cache()` for mysql.
  (Directory/sqlite connectors re-read the source each time, so no clear needed.)

Note the exact function name is **`pg_clear_cache`** (not `postgres_clear_cache`,
which the extension *suggests* but does not define).

**Recognise it next time.** Anything that re-materialises a view on the
long-lived per-tenant connection (validate, rebuild) must assume the alias is
already attached and the remote catalog is cached. Test DB connectors against a
**real** server (testcontainer) — `crates/escurel-index/tests/sql_view_postgres.rs`
is the guard; the directory-connector tests structurally cannot catch either bug.
