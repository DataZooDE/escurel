# `ducklake_expire_snapshots(older_than => NULL)` crashes; the current snapshot is never expired

## Symptom

Implementing snapshot GC/retention (`gc_lake_snapshots`, DuckLake PR 7)
against a scratch lake via the `duckdb` CLI: calling
`CALL ducklake_expire_snapshots('lake', older_than => NULL)` does not
return an empty-result no-op — it crashes the `ducklake` extension with
an **internal error** (`GetValueInternal on a value that is NULL`), a
full C++ stack trace, and a non-zero exit. This is exactly the shape you
get if you compute a "keep N most recent" cutoff naively (`ORDER BY
snapshot_id DESC LIMIT 1 OFFSET keep - 1`) against a lake that has fewer
than `keep` snapshots — the offset query legitimately returns zero rows,
and calling the function with that `NULL` blows up rather than being a
no-op.

## Cause

`ducklake_expire_snapshots`'s `older_than` parameter is untyped-NULL-
unsafe in the extension build tested (`duckdb` v1.5.4 / Variegata, the
`ducklake` extension pulled via `INSTALL ducklake`). The function's
signature accepts `dry_run BOOLEAN`, `older_than TIMESTAMP WITH TIME
ZONE`, `versions UBIGINT[]` (confirmed via the binder's "Candidates"
error message when passing a bogus named parameter) — there is no
built-in "keep the N most recent snapshots" mode, so a caller must
compute the cutoff timestamp itself and MUST guard the "fewer snapshots
than the retention target" case before calling.

Separately (useful, not a bug): `ducklake_expire_snapshots` never prunes
the **current** (newest) snapshot regardless of how `older_than`
compares to it — verified by expiring with `older_than => now()` against
a 5-snapshot lake and observing the newest snapshot survive. So actual
snapshot counts after a GC pass can be `keep` (typical) rather than
never dip below 1, even when the caller passes an aggressive cutoff.

## Fix

`crates/escurel-index/src/snapshot/lake.rs::gc_lake_snapshots` computes
the cutoff with a bounded `OFFSET` query and treats "no row" (fewer
snapshots than `keep`) as `Ok(0)` — never calling
`ducklake_expire_snapshots` with a `NULL` `older_than`:

```sql
-- 1. Cutoff: the `keep`-th-from-newest snapshot's timestamp, cast to a
--    bindable VARCHAR (duckdb-rs binds TIMESTAMPTZ awkwardly as a raw
--    param; round-tripping through VARCHAR + an explicit ::TIMESTAMPTZ
--    cast on the way back in is simplest and was verified to preserve
--    exact retention counts).
SELECT CAST(snapshot_time AS VARCHAR) FROM ducklake_snapshots('lake')
  ORDER BY snapshot_id DESC LIMIT 1 OFFSET <keep - 1>;
-- Zero rows → fewer than `keep` snapshots exist → skip GC, Ok(0).

-- 2. Only when step 1 returned a row:
CALL ducklake_expire_snapshots('lake', older_than => ?::TIMESTAMPTZ);
CALL ducklake_cleanup_old_files('lake', cleanup_all => true);
```

Verified interactively end-to-end (six inserts → six snapshots, `keep =
2`): the two-call sequence above prunes snapshots 0-3 and leaves exactly
4 and 5; `ducklake_cleanup_old_files(cleanup_all => true)` afterwards
reports 0 remaining orphaned files (they were reclaimed).

## How to recognise it next time

An internal-error crash (not a clean `Binder Error`) out of the
`ducklake` extension when calling `ducklake_expire_snapshots`, with a
stack trace bottoming out in `GetValueInternal` — check whether
`older_than` is being passed as SQL `NULL`. Guard the "not enough
snapshots yet" case before the call, the same way `gc_lake_snapshots`
does.
