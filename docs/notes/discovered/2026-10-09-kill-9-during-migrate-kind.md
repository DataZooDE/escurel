# `kill -9` in the middle of `migrate-kind` must leave a tenant that still knows it is unmigrated

**Symptom.** `escurel admin migrate-kind --apply` rewrites pages, open drafts and CRDT snapshots and then
rebuilds the index. An OOM kill, a node loss or an orchestrator's SIGKILL between the first rewrite and
the end of the rebuild used to leave a lane whose pages were already `kind:` (so the boot-time legacy scan
found nothing to complain about) while the index had never been rebuilt from them: the tenant booted
"healthy" over a half-migrated store.

**Fix.** The migration writes a durable marker `meta/migrate-kind.pending` in the lane BEFORE the first
rewrite and removes it only after pages, drafts, snapshots AND the rebuild all succeeded
(`crates/escurel-index/src/migrate_kind.rs`, `MIGRATION_MARKER_PATH`). A tenant that boots with the marker
present is QUARANTINED exactly like one with `type:` pages (`/readyz` says `migration_pending`,
`escurel_migration_pending 1`), and re-running `migrate-kind --apply` is idempotent: it finds what is
left, finishes, clears the marker. The apply also runs in a spawned server task so a client that gives up
(the CLI's old 60 s transport timeout, a killed shell) does not cancel it.

**How to recognise it.** `x-escurel-quarantined: 1` on `/readyz` with `notices: [quarantined,
migration_pending]` and no `type:` page listed; the lane holds `meta/migrate-kind.pending`.
Do NOT delete the marker by hand: run the migration again.

**Regression test.** `crates/escurel-server/tests/suite/migrate_kind_sigkill.rs`: a literal `kill -9`
of the real server binary at a point chosen by watching the marker, then a NEW process over the same
lane must boot quarantined and a second apply must finish. The default sweep kills once per phase;
`ESCUREL_SIGKILL_FULL=1` (nightly in `.github/workflows/live.yml`) runs all ten kill points.
