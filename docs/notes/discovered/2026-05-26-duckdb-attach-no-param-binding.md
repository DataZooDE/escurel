# DuckDB `ATTACH` has no parameter binding — validate the source string

**Symptom.** Wiring `EscurelAdmin.attach_external` (the external-lane
RPC) tempted the obvious "bind the path as a prepared-statement
parameter" approach:

```sql
ATTACH ? AS ? (READ_ONLY)
```

DuckDB rejects this. `ATTACH`'s database-path and alias positions are
**not** bindable: they must be SQL literals/identifiers spliced into
the statement text. There is no `params!`/`ToSql` escape hatch for
them the way there is for `WHERE x = ?`.

**Fix.** `Indexer::attach_external(alias, source)` builds the SQL by
`format!`-ing both positions in, so the injection boundary moves to
*validation of the inputs* rather than driver-side binding:

- `alias` is **derived**, never user-supplied: `derive_attach_alias`
  takes the source's file stem and collapses it to `[A-Za-z0-9_]`,
  prefixing `ext_` if it would otherwise start with a digit.
  `is_valid_attach_alias` re-checks it is a safe unquoted identifier.
- `source` is rejected by `is_safe_attach_source` if it contains any
  quote, backslash, semicolon, backtick, or control char — the
  characters that could close the `'...'` literal, stack a second
  statement, or smuggle an escape. The gRPC handler checks this
  before the SQL is ever built; the indexer re-checks defensively.

`attach_external` is admin-only, but the validation is unconditional:
admin tokens are still attacker-reachable if an admin's credentials
leak, and the cost of the allow-list is trivial.

**How to recognise it next time.** Any new RPC that takes a *path*,
*URI*, *catalog name*, or *pragma value* and feeds it to DuckDB DDL
(`ATTACH`, `INSTALL`, `LOAD`, `COPY ... TO '<path>'`, `SET ...`) is in
the same boat: the value usually can't be bound, so it must be
allow-list-validated before it reaches the statement text. Reach for
`is_safe_attach_source` / `derive_attach_alias` in `escurel-index` as
the pattern.

**v1 scope.** We use DuckDB's **native** `ATTACH` of a second DuckDB
file (read-only) — no DuckLake/Iceberg extension. The spec
(`storage.md` §"external lane") describes a DuckLake catalog; native
ATTACH is the v1 escape hatch that makes `[[query::*]]` over an
external catalog work today without the extension dependency. The
catalog alias is returned as `AttachExternalResponse.source_id`.
