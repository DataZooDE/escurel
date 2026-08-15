# Attached-Postgres gotchas: `ON CONFLICT` is not atomic, and out-of-band DDL needs `pg_clear_cache()`

Found while fixing the shared chat/events tables' tenancy + idempotency
(index/shared-events-tenancy).

## Symptom 1: `ON CONFLICT DO NOTHING` still raises `duplicate key`

Two replicas appending the same `msg_id` concurrently into the shared
`chat_pg.escurel_chat_messages` table intermittently failed with

```
Failed to copy data: ERROR: duplicate key value violates unique constraint
CONTEXT: COPY escurel_chat_messages, line 1
```

even though the INSERT carried `ON CONFLICT (msg_id) DO NOTHING`.

**Why.** The DuckDB Postgres connector does not translate the conflict
clause into a Postgres upsert. It resolves `ON CONFLICT` with its own
pre-check and then ships the row via `COPY` — so a *sequential*
redelivery is correctly skipped, but the loser of a genuinely
*concurrent* cross-connection race still hits the PK at COPY time.

**Fix / recognition.** Treat `ON CONFLICT` over an attached Postgres
table as check-then-insert, not an atomic upsert. If the contract is
"second writer echoes the stored row", catch the duplicate-key error
(string-match on `duplicate key value violates unique constraint`) and
read the stored row back — see `chat.rs::append_chat_message`. The
same caveat applies to `events.rs::capture_event`'s dedup: its
`ON CONFLICT` handles re-delivery, not simultaneity (the runner's
SQLite ledger is what makes runs exactly-once).

Corollary of the same COPY implementation (already documented in
`events.rs`, bites again whenever a new insert path is added): every
column of the target relation is materialised, so a column omitted from
the INSERT arrives as an explicit NULL instead of taking the Postgres
`DEFAULT` — write `created_at` explicitly.

## Symptom 2: `ALTER TABLE` via `postgres_execute` is invisible to DuckDB

Migrating the shared events table's PK in place
(`postgres_execute('events_pg', 'DO $$ … ALTER TABLE … ADD PRIMARY KEY
(tenant, event_id) …')`) succeeded on the Postgres side, but the next
insert on the same connection failed to bind:

```
Binder Error: The specified columns as conflict target are not referenced
by a UNIQUE/PRIMARY KEY CONSTRAINT or INDEX
```

**Why.** DuckDB caches an attached table's schema — constraints
included — and `postgres_execute` runs behind that cache's back.

**Fix.** `CALL pg_clear_cache();` after any out-of-band DDL, before the
first statement that depends on it (`snapshot/events_pg.rs::
attach_events_pg`).
