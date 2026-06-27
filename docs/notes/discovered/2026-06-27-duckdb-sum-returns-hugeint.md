# DuckDB `SUM()` over integers returns HUGEINT → JSON string

**Symptom.** A `query_instance` (or any stored query) report like
`SELECT category, SUM(amount) AS total FROM … GROUP BY category` comes back
with `total` rendered as a **JSON string** (`"50"`), not a number. Asserting
`row["total"].as_i64()` returns `None` and the test panics on `.unwrap()`.

**Why.** DuckDB widens `SUM()` of an integer column to **`HUGEINT`** (128-bit)
to avoid overflow. Our `duck_to_json` (in `escurel-index/src/query.rs`) maps
`DuckValue::HugeInt(n)` to `Value::String(n.to_string())` on purpose —
`HUGEINT`'s range exceeds `i64`, so it cannot be represented as a JSON number
without possible loss. The same applies to `COUNT(*)`? No — `COUNT` returns
`BIGINT`; it's specifically `SUM`/`AVG`-style aggregates over integers that
promote to `HUGEINT`. Unsigned `UBIGINT` and `HUGEINT` are the two integer
types we string-encode.

**Fix / how to recognise it.** This is **not** a bug — it's the lossless
encoding contract. The report author should cast the aggregate down to a JSON
number type when the value is known to fit:

```sql
SELECT category, SUM(amount)::BIGINT AS total FROM {{target}} GROUP BY category
--                          ^^^^^^^^ HUGEINT → BIGINT → JSON number
```

If you see a numeric column arriving as a quoted string in a report or a
stored-query result, suspect an un-cast `SUM`/aggregate and add `::BIGINT`
(or `::DOUBLE`) in the report SQL. The integration tests
(`crates/escurel-index/tests/query_instance.rs`,
`crates/escurel-server/tests/query_instance_tools.rs`) cast deliberately so
their `as_i64()` assertions hold.
