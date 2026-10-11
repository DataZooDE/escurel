# A statement-timeout test that raced the machine: slow BY CONSTRUCTION, not "slow enough on my laptop"

**Symptom.** `rows_paging_keys::reading_one_row_is_under_the_same_statement_timeout_as_listing` failed in CI on
PRs that did not touch it (three times, #684/#685/#689 era), with `expand` of a row page answering normally
instead of the worded timeout error. It passed locally and in `scripts/flaky-hunt.sh` on an idle machine
(0/100). #683 had already fixed a REAL hole (a watchdog interrupt that lands while `prepare` is still binding is
lost, so the watchdog now repeats it every 2 ms); the flake survived because the cause was in the test.

**Cause.** The test gave the source 2M rows with ASCENDING keys and asked for the ~2,000th key with a 1 ms
budget. `rows_get` runs `SELECT ... WHERE CAST(k AS VARCHAR) = ? LIMIT 1` over a parquet-backed view. Ascending
keys let the parquet min/max statistics skip every row group but the first, and `LIMIT 1` stops at the first
match: a few milliseconds of work in total. Whether the watchdog's interrupt lands inside those few ms depends
on how fast the core is, so the outcome raced the machine (the contract, "a read that cannot finish within the
budget ends in a worded error", was never violated: the read finished within the budget).

**Reproduce.** Pin the test binary to one core: `taskset -c 0 <suite-binary> --exact <test>` x100 failed 53
times; unpinned on an idle 32-core machine 0/100.

**Fix.** The test's source must be slow by construction: keys are a permutation of 0..2M (`i * 7919 mod
2000003`, a bijection: 2000003 is prime), so every row group spans (almost) the whole key range and none can be
pruned, and the lookup asks for the key of the LAST row written, so `LIMIT 1` reads the whole file. Measured on
the same 2M rows: ascending key 14-29 ms, permuted last key about 75 ms of multi-threaded scan (more on one
core), against the 1 ms budget. After: 0/200 pinned to one core, 0/500 idle, 0/200 under 32 busy loops.

**How to recognise it next time.** A "must time out" test whose workload is a point lookup, a `LIMIT`, or any
query a statistics-aware engine can shortcut. Check `EXPLAIN ANALYZE` for rows scanned, and run the test pinned
to one core (`taskset -c 0`): a test that is only slow enough on a fast machine fails there first.
