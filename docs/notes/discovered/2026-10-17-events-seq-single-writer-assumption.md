# The events `seq` is minted by "read max, then insert": safe only for one writer per tenant

**Found by:** the 2026-10-17 security pass (finding F4), then analysed here. No change made.

## Symptom (potential)

On the attached-Postgres events table, `capture_event` mints the next `seq` in two steps
(`crates/escurel-index/src/events.rs`, ~600-620): a native `SELECT COALESCE(max(seq), 0) + 1 FROM
postgres_query(…)`, then an `INSERT … seq = <that value>`. `(tenant, seq)` has an index but not a UNIQUE one. Two
processes capturing for the same tenant at the same moment could read the same `max` and both insert it; a tail
cursor (`seq` strictly greater than the last one seen) would then step over the second event.

## Why it does not bite today

* Inside one gateway the capture runs under the indexer's connection lock, so two captures never interleave.
* Two gateways on one tenant is the misconfiguration the single-writer lease (#371) refuses at boot: the loser never
  attaches as a writer. Readers do not capture.
* The same two-step shape existed before the Postgres-native read (#729): it was a scalar subquery inside the
  `INSERT`, equally non-atomic across processes.

So the race needs a second writer the architecture forbids. It is an assumption worth writing down, not a live bug.

## If it ever has to hold under concurrency

Either of these, in this order of preference:

1. `INSERT … SELECT COALESCE(max(seq), 0) + 1 …` executed natively in Postgres (`postgres_execute`) so the read and
   the insert are one statement, **plus** a UNIQUE index on `(tenant, seq)` so a collision fails loudly (retry) instead
   of minting a duplicate.
2. A per-tenant Postgres sequence.

Do **not** add the UNIQUE index before checking existing data: a table that ever had a duplicate `seq` (for instance
from the legacy-heartbeat era or a manual import) refuses the `CREATE UNIQUE INDEX` and would fail the attach. Find
duplicates first: `SELECT tenant, seq, count(*) FROM escurel_events GROUP BY 1, 2 HAVING count(*) > 1`.
