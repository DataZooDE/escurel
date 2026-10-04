# DuckDB's interrupt does not cancel a scan blocked in the `postgres` extension

**Symptom.** A `rows` skill over a slow Postgres relation ignored `with_rows_query_timeout(2s)`: the
`list_instances` call ran the view's full 60 s (a `pg_sleep` per row) before answering. The statement
timeout from the rows engine (`with_statement_timeout`, a watchdog calling
`Connection::interrupt_handle().interrupt()`) works for DuckDB-native scans (parquet, JSON, SQLite) and
is a no-op once the scan is waiting inside the postgres extension's libpq call.

**Fix.** Make the SERVER enforce it: for a Postgres attach the DSN gains the libpq
`options=-cstatement_timeout=<ms>` parameter (`sql_view::with_server_statement_timeout`; no spaces or
quotes, so it passes `is_safe_sql_fragment` and works in key=value and URI DSNs; an `options` the
operator already set wins). The watchdog stays as the backstop for every other connector, and an error
that arrives past the deadline is reported as the timeout (server and watchdog fire at the same
instant, so either may win).

**Recognise it next time.** A timeout test against a real Postgres that takes the full duration of the
slow statement instead of the configured limit. It only shows against a real Postgres: a SQLite or
parquet fixture is interrupted correctly. Test: `sql_rows_postgres::a_slow_source_is_interrupted_*`
(`--features live-postgres`, needs Docker).

**Also learned (same work).** `write_back` against a database goes through a SECOND, short-lived
read-write DuckDB connection, never the indexer's persistent read-only one; and SQLite
`database is locked` and Postgres connection errors are classified transient (retried, then
dead-lettered) while constraint and cast errors are final.
