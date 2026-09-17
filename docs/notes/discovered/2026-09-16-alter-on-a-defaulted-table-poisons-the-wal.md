# An `ALTER TABLE … ADD COLUMN` on a table with a function-valued DEFAULT poisons the WAL

**Date:** 2026-09-16 · **Found while:** adding `drafts.changeset_id` (#509 §1).

## Symptom

Five `escurel-index` tests failed — every one that reopens the same DuckDB
file a second time (`index_roundtrip::full_roundtrip_audit_rebuild`,
`…::rebuild_clears_stale_rows_for_deleted_markdown`,
`…::update_page_persists_frontmatter_as_json`, `…::update_page_persists_wikilinks`,
`…::update_page_populates_dense_vec_with_embedder_output`):

```text
INTERNAL Error: Failure while replaying WAL file "…/escurel.duckdb.wal":
Calling DatabaseManager::GetDefaultDatabase with no default database set
```

The stack is `WriteAheadLogDeserializer::ReplayAlter` → `DuckTableEntry::AddColumn`
→ `Binder::BindDefaultValues`.

## Cause

DuckDB re-binds **every** column default of a table when it replays an
`ALTER TABLE … ADD COLUMN` from the WAL. `drafts.created_at` is
`DEFAULT CURRENT_TIMESTAMP`, which resolves through a catalog lookup the
replay context does not have — so the entry is unreplayable.

The failure mode is the nasty shape: the process that ran the ALTER keeps
working, and only the **next** process to open the file fails to start.

## Fix

Run the ALTER only on the boot that actually adds the column, then
`CHECKPOINT` so it is folded into the database file and the WAL is
truncated — there is then nothing left to replay. See
`Migrator::ensure_draft_changesets`, which is a copy of the shape
`Migrator::ensure_write_attribution` already used for `crdt_ops.principal`
(escurel#357) and documents the same trap.

```rust
let present: i64 = conn.query_row(
    "SELECT count(*) FROM information_schema.columns \
     WHERE table_schema = 'main' AND table_name = 'drafts' \
       AND column_name = 'changeset_id'",
    [], |row| row.get(0))?;
if present == 1 { return Ok(()); }
conn.execute_batch(STAGE_14_DRAFT_CHANGESETS)?;
conn.execute_batch("CHECKPOINT;")?;
```

## How to recognise it next time

Before adding an `ensure_*` that ALTERs a table, check that table's DDL for
a function-valued `DEFAULT` (`CURRENT_TIMESTAMP`, `now()`, `nextval`, …). If
there is one, the migration MUST be presence-checked + checkpointed rather
than run unconditionally on every connection. Tables that carry one today:
`drafts` (`created_at`), `crdt_ops` (`applied_at`), `events` (`at`),
`chat_messages` (`at`). `blocks` and `pages` declare no defaults, which is
why `0007_block_context.sql` never hit this.
