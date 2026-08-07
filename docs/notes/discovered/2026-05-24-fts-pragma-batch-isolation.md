# `PRAGMA create_fts_index` cannot see DDL from its own `execute_batch`

**Date:** 2026-05-24 (note written 2026-08-07 — see *Why this is late*)
**Scope:** `escurel-index` — `Migrator::up`, `crates/escurel-index/src/schema.rs`

## Symptom

Running the v1 schema as a single `conn.execute_batch(...)` — every
`CREATE TABLE` plus `PRAGMA create_fts_index('blocks', ...)` in one string —
fails: the PRAGMA reports that the target table does not exist, even though
the `CREATE TABLE blocks` statement is a few lines above it in the same
batch and the batch is well-formed SQL.

## Cause

The `fts` extension's `create_fts_index` PRAGMA resolves its target table in
a **fresh context** rather than against the uncommitted state of the
statement stream it is executing in. Within one `execute_batch` the preceding
DDL has not been committed, so the catalog the PRAGMA consults does not yet
contain `blocks`.

This is specific to the extension's PRAGMA. Ordinary DDL and DML in the same
batch see each other exactly as expected, which is what makes the failure
read as an impossible one.

## Fix

Split the migration into staged batches, so that an intermediate commit makes
the catalog visible before the PRAGMA looks it up. `Migrator::up` issues
`STAGE_1_AUTOLOAD`, `STAGE_2_TABLES_AND_INDEXES`, then `STAGE_3_FTS`, and so
on as separate `execute_batch` calls. The staging is load-bearing, not
stylistic — collapsing the stages back into one batch reintroduces the
failure.

Note that the stages have since acquired a second, unrelated reason to exist:
they let later schema additions (`chat_messages`, scenarios, events, group
members, external credentials, pack subscriptions, provenance) be appended
without disturbing the core tables. Do not take that as licence to merge the
early stages — stages 2 and 3 must stay apart.

## How to recognise it next time

A DuckDB extension PRAGMA claiming that an object created earlier in the same
batch does not exist. The tell is that the same statements succeed when run
one at a time from a REPL, and fail only when concatenated — the opposite of
an ordinary SQL error, which does not care how the statements were delivered.

Related and distinct: `2026-05-24-fts-no-refresh-pragma.md` (there is no
`refresh_fts_index` PRAGMA at all), `2026-05-24-duckdb-vss-fts-autoload.md`
(loading the extensions), and `2026-05-24-vss-hnsw-experimental-persistence.md`
(the HNSW-on-file-backed-DB setting, which is why stage 2 is also separate).

## Why this is late

`schema.rs:174` has cited this file since the migration was written; the file
was never committed. The mechanism survived only as the code comment that
points here, so anyone following the citation found nothing and anyone
tempted to tidy the stages into one batch had only that comment to stop them.
Found by sweeping every `docs/notes/...` path cited from Rust source against
what exists on disk:

```sh
grep -rhoE '`?docs/notes/[A-Za-z0-9_./-]+\.md' --include=*.rs crates/ \
  | tr -d '`' | sort -u | while read -r p; do [ -e "$p" ] || echo "MISSING: $p"; done
```

Worth re-running when a note is moved or renamed. The equivalent sweep for
the consumer skill is already in `CLAUDE.md`; this one covers code comments,
which nothing checked.
