# Moving events, drafts and chat from Postgres to the lake

`ESCUREL_EVENTS_BACKEND` and `ESCUREL_CHAT_BACKEND` choose where the append surfaces live: the catalog database
(`postgres`, the default) or the DuckLake itself (`ducklake`). Drafts have no knob of their own: they follow the events
selector. **Changing the variable alone does not move anything**: the lake tables start empty and the Postgres rows
(inbox items, pending drafts, audit events, chat) become unreachable. `escurel-server events-copy` is the supported,
lossless copy.

Facts behind the choice (ADR-0009 Phase B, `storage.md`): `docs/notes/discovered/2026-10-17-events-copy-postgres-to-lake.md`.

## What it does

`escurel-server events-copy [--apply] [--tables events,drafts,chat]`, configured by the same `ESCUREL_*` environment as
the writer (`ESCUREL_TENANT`, `ESCUREL_INDEX_BACKEND=ducklake`, `ESCUREL_DUCKLAKE_*`, the object-store or gdrive
variables). The source is the Postgres database of the lake catalog; the destination is the lake.

- **Per tenant.** Only `ESCUREL_TENANT`'s rows move; the shared Postgres tables hold every tenant of the database.
- **Faithful.** `event_id`, `seq`, `at_ts`, `kind`, `status`, the lineage columns (`root_event_id`, `run_id`), draft ids
  and chat `msg_id`/`ts` are copied as stored. The `seq` of the lake continues after the copied maximum.
- **Idempotent.** A row is copied only if no destination row has its key. Re-running copies what is missing and nothing
  twice, so a run that died between tables is simply run again.
- **Never destructive.** The Postgres tables are attached read-only; nothing is deleted or modified there. A destination
  row that exists but *differs* from its source is reported as `diverged` and refuses the apply: the tool never overwrites.
- **Verified.** After the insert each table's row count and checksum (over every copied column) must equal the source's.
- **Audited.** A run that copied rows writes one `escurel:events-copy` system event (counts in the body) into the lake.
- **Dry run by default.** Without `--apply` it only reads and creates nothing, not even the lake tables.
- **Stopped writer.** An apply takes the single-writer lease and refuses while a writer holds it (like `lake-upgrade`).
  A dry run needs no lease.

The report is JSON on stdout: per table `source_rows`, `already_present`, `diverged`, `to_copy`, `copied`, and the two
checksums.

## Runbook

1. **Backup.** Take a point-in-time restore point of the catalog database (the source stays untouched, but the lake
   gains tables and snapshots) and note the lake's current snapshot.
2. **Rehearse** on a copy of the catalog and of the lake data (`docs/deploy/kind-migration.md` §rehearsal) with the real
   environment shape; record the timings. On Google Drive each lake write costs seconds: size the deadline from the
   rehearsal, not from the row count.
3. **Stop the writer** (the single-writer Deployment is scaled to zero / stop-first).
4. **Dry run:** `escurel-server events-copy`. Expect `diverged: 0` everywhere and `to_copy` equal to `source_rows` on a first run.
5. **Run the job:** `scripts/events-copy-job.sh` does dry run, apply and an independent verification dry run, and exits 0
   only when nothing is left to copy. Env: `ESCUREL_TENANT`, the writer's lake environment, `ESCUREL_JOB_DEADLINE_SECS`
   (default 3600), optionally `ESCUREL_EVENTS_COPY_TABLES` (default `events,drafts,chat`). It starts no server, so no
   auth or webhook environment is involved.
6. **Switch** `ESCUREL_EVENTS_BACKEND=ducklake` and `ESCUREL_CHAT_BACKEND=ducklake` on the writer (and readers) and boot.
7. **Verify** through the product: the inbox, a draft's review queue and a chat group show the same rows as before.
8. **Rollback** is switching the two variables back: the Postgres rows were never touched. Rows written to the lake after
   the switch are not copied back; the audit event marks the copy time.

## After the switch (Drive-backed lakes)

Every capture is then a synchronous lake write (one Parquet object and one snapshot), and the publish task rewrites the
append tables each cycle: plan the lake settings (publish tick, compaction, retention) before the window.
