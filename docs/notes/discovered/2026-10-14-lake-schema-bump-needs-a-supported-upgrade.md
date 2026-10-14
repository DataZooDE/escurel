# A lake schema bump without an upgrade path blocks the cutover (and the manual fix is a hidden step)

**Symptom.** The lab cutover on 2026-10-13 booted the new image and died before `migrate-kind` could run:
`lake schema_version 9 != reader schema_version 10 (lake incompatible with this reader)`. The lake's
`escurel_manifest` pins the schema version of the writer that published it, and the adopt gate is fail-closed
(#563). `Migrator::SCHEMA_VERSION` is a manual constant: 9 -> 10 (#653) only added one nullable column,
`events.revision_binding_sha256`, which `attach_events_lake` adds by itself. No published corpus table changed
shape, yet nothing let the new build open the old lake; the only way across was a hand-run
`UPDATE lake.escurel_manifest SET schema_version = 10` through a DuckDB CLI (rehearsed 2026-10-14, 4 s).

**Fix.**
- `Migrator::LAKE_ADOPTS_FROM` lists the older versions this build adopts as-is. A bump that changes a
  published table's shape must NOT be listed (it then needs a real migration).
- `escurel_index::snapshot::lake_upgrade` / `escurel-server lake-upgrade [--dry-run]` re-pin the manifest:
  idempotent (`already_current`), refuses an unknown older version and a NEWER lake with a non-zero exit and
  nothing written, takes the single-writer lease on a Postgres catalog.

**Recognise it next time.** A boot that fails with `lake schema_version N != reader schema_version M`. Check
whether the bump changed a published table; if not, list N in `LAKE_ADOPTS_FROM`; if so, write the migration.
Never drop and rebuild the lake: events, chat and drafts live only there.

## Changelog lines for the release notes
- New: `escurel-server lake-upgrade [--dry-run]` re-pins a DuckLake manifest that is one declared-compatible
  schema version behind (schema 9 -> 10). The server also adopts such a lake at boot without any manual step.
