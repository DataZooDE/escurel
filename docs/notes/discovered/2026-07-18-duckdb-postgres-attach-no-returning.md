# DuckDB's Postgres connector rejects `INSERT … RETURNING` on an attached table

**Symptom.** An `INSERT INTO <attached_pg_alias>.<table> (...) VALUES (...)
RETURNING <expr>` against a table reached via `ATTACH '<dsn>' AS alias
(TYPE postgres)` fails at bind time:

```
Binder Error: RETURNING clause not yet supported for insertion into Postgres table
```

This is unrelated to the `ducklake:postgres:` catalog protocol (`lake.rs`) —
it hits the plain `TYPE postgres` connector any writable cross-database
insert uses (DuckLake PR 8's chat re-homing: `ATTACH … AS chat_pg (TYPE
postgres)`, read-write, NOT `READ_ONLY`).

**Fix.** Don't rely on `RETURNING` to read back a server-resolved value
(here: `COALESCE(TRY_CAST(? AS TIMESTAMP), CURRENT_TIMESTAMP)` for a NULL
`ts` input). Resolve it FIRST with a plain scalar `SELECT` (touches no
table — it's an ordinary DuckDB query, not an attached-table statement),
then bind the already-resolved value straight into the `INSERT` and run it
with `conn.execute` (not `conn.query_row`). See
`crates/escurel-index/src/chat.rs::append_chat_message`'s
`chat_tenant_scope().is_some()` branch.

**How to recognise it next time.** Any `INSERT … RETURNING` (or `UPDATE …
RETURNING` / `DELETE … RETURNING`) issued against a table qualified by an
alias created with `ATTACH '<dsn>' AS alias (TYPE postgres)` — read-write
or not — will hit this. The local single-file `chat_messages` table (no
attach involved) is unaffected; only cross-database writes through the
Postgres connector are. Probed empirically 2026-07-18 against a Postgres
17 testcontainer, DuckDB via the workspace's pinned `libduckdb-sys`
release (see `.cargo/config.toml`).
