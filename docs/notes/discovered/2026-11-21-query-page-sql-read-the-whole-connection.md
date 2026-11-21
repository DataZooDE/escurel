# A query page's SQL could read the whole indexer connection

**Symptom.** A non-admin caller ran `query_instance` on a page whose SQL was
`SELECT e.secret FROM {{target}} CROSS JOIN external_credentials e` and got
the registered DSN back. The same worked for other instances' `vw_` views,
`pages`, `sql_view_search_text`, `read_text('/etc/…')` and `duckdb_secrets()`.

**Why.** The ACL check is on the *target*. The SQL then ran verbatim, after
`{{target}}` substitution, on the indexer's single connection, which holds
everything. Requiring the placeholder (an earlier fix) proved only that the
target was *named*, not that nothing else was.

**Fix.** `query_gate::check` walks DuckDB's own parse tree
(`json_serialize_sql`) default-deny before `prepare`. Base tables must be the
target or a CTE in scope, table functions must be the generators `range`,
`generate_series` or `unnest`, there must be no state-reading or
foreign-qualified functions, and there must be exactly one `SELECT`. Three
details mattered:

- **Bind the text as `?::VARCHAR`.** An untyped parameter makes
  `json_serialize_sql` fail, and the gate then refuses everything.
- **The parser itself qualifies desugared builtins.** A list literal `[1, 2]`
  becomes `list_value` with schema `main`. Allow `system.main`, nothing else.
- **CTE names are scoped.** A global CTE-name set would let
  `(WITH pages AS (…) SELECT * FROM pages) a, pages p` read the real `pages`.

The gate also runs before `prepare` for a second reason: preparing
multi-statement text already executes every statement but the last.

**Recognise it next time.** An ACL decision about object A, followed by code
that runs caller-supplied text able to name objects B…Z. Check what the text
can *reach*, not only what it must *contain*.
