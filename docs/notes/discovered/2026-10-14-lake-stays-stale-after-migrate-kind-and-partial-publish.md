# migrate-kind left the lake stale; a cancelled rebuild could publish a partial corpus

Found by the lab-cutover rehearsal of 2026-10-14 (clone of the lab lake, 183 pages / 988 snapshots).

**F1: `migrate-kind --apply` did not publish the lake.** The apply rewrote the lane and re-derived the local
index, then the job stopped the server (SIGTERM does not publish). The next writer boot rebuilt from the
(`kind:`) lane and then `adopt_lake_for_writer` REPLACED the local index with the stale lake corpus (all
`type:`). The quarantine check had already run on the lane, so the tenant was NOT quarantined
(`escurel_tenant_quarantined 0`) and served `type:` frontmatter silently; the job would have reported OK.
Fix: the server quarantines the tenant after the adopt when the adopted pages carry `type:`
(`Indexer::quarantine_stale_adopted_lake`), and `migrate_kind --apply` publishes the lake itself on a lake
writer (`lake_published` in the report; a publish failure fails the call, a retry is idempotent).

**F2: `admin rebuild` was not cancel-safe.** The CLI's client deadline was a fixed 60 s; the server rebuild ran
in the request future, so a client that gave up dropped it AFTER the truncate and left the served index
partial. A `publish_snapshot` then published 91 of ~183 pages over the populated lake (the only guard refused
an EMPTY corpus). Fix: the rebuild runs in a spawned task (a client that disconnects no longer cancels it),
`Indexer::rebuild_incomplete` is set from the truncate until the last page is back and a publish refuses while
it is set, `publish_lake` refuses a corpus under half the lake's pages (lakes of 10+ pages; `allow_shrink`
for a deliberate mass delete), and `escurel admin rebuild` takes `--timeout-secs` with no default deadline.

**Recognise it next time.** After a migration on DuckLake: `escurel_tenant_quarantined 0` but `list_instances`
shows `type`; a lake page count that fell between two publishes; a `publish_snapshot` error naming
`allow_shrink` or "a rebuild did not finish".

## Changelog lines for the release notes
- Changed: `migrate_kind --apply` on a DuckLake writer publishes the migrated corpus to the lake (report field
  `lake_published`); a writer that adopts a lake still holding `type:` pages boots QUARANTINED instead of
  serving it. `scripts/migrate-kind-job.sh` verifies with a second boot.
- Changed: `publish_snapshot` refuses a partial index and a corpus under half the lake's pages; new optional
  argument `allow_shrink`. `rebuild` keeps running when its client gives up; `escurel admin rebuild
  --timeout-secs`.
