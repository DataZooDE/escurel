# A statement timeout that fires during `prepare` is lost

**Symptom.** `rows_paging_keys::reading_one_row_is_under_the_same_statement_timeout_as_listing` (a 1 ms budget over a
2M-row source) passed locally and failed in CI: the single-row read simply came back, no `interrupted` error.

**Cause.** `with_statement_timeout` (`crates/escurel-index/src/backend/rows.rs`) arms a watchdog thread that calls
`Connection::interrupt_handle().interrupt()` after the budget. DuckDB's interrupt only reaches a statement that is
RUNNING. The closure starts with `conn.prepare(..)` (binding: reading parquet metadata, a few ms); a budget that
expires there interrupts nothing, and the statement then executes to the end. The earlier tests (listing sorts 2M
keys) were long enough that the interrupt almost always landed in the execute phase.

**Fix.** After the deadline the watchdog repeats the interrupt every 2 ms until the closure returns (the
join already bounds that to one more tick, so a late interrupt still cannot hit the next statement).

**Recognise it next time.** A "timeout" test that is green on a loaded machine and red on a fast one, with a 1 ms
budget: the budget expired before the statement started. Production budgets are seconds, so the practical risk
was small, but the guarantee ("a slow source cannot hold the connection") must not depend on timing.
