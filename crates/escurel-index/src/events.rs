//! Events / inbox surface (M7 — Event-sourcing surface).
//!
//! Events are the *dynamic* input of escurel's memory triad
//! (Events · Skills · Instances). They live in the `events` table —
//! a global queue tightly bound to the page model without being pages:
//! each event's `label_skill` links to the **skill** that knows how to
//! process it, and `instance_page_id` links to the **instance** it
//! belongs to once an (external) agent has processed it.
//!
//! Surface:
//! - [`Indexer::capture_event`] — append an event (lands in the inbox).
//! - [`Indexer::list_inbox`] — unprocessed events (`status = 'inbox'`).
//! - [`Indexer::list_events`] — an instance's processed event history.
//! - [`Indexer::assign_event`] — assign an inbox event to an instance
//!   (→ `processed`), the (simulated) agent's act of folding it into state.

use duckdb::params;
use ulid::Ulid;

use crate::indexer::{Indexer, IndexerError};

/// Table name for the shared attached-Postgres events table (DuckLake
/// PR 9, Phase B). Lives here (not `snapshot::events_pg`) because
/// `events.rs` owns the events concept; `snapshot::events_pg` imports it
/// back for the `CREATE TABLE` DDL so the name is defined exactly once —
/// mirrors `chat.rs::CHAT_PG_TABLE_NAME` / `snapshot::chat_pg`.
pub const EVENTS_PG_TABLE_NAME: &str = "escurel_events";

/// Which physical table [`Indexer`]'s events methods (`capture_event` /
/// `assign_event` / `list_events` / `list_inbox`) read and write.
///
/// `Local` (the default `Indexer::new` construction) is today's
/// single-file behaviour, byte-identical: the per-tenant `events` table,
/// no `tenant` column (tenancy is implicit — one DuckDB file per
/// tenant). `AttachedPostgres` (DuckLake PR 9) points every ducklake
/// replica — writer AND every reader — at ONE shared, writable Postgres
/// table (`snapshot::attach_events_pg`), scoped by an explicit `tenant`
/// column since the physical table is no longer implicitly
/// single-tenant. Mirrors [`crate::chat::ChatBackend`] exactly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EventsBackend {
    /// The local per-tenant `events` table.
    Local,
    /// A read-write table in the LAKE (Parquet on the object store,
    /// `snapshot::attach_events_lake`). Same row shape and same
    /// every-replica-writes model as `AttachedPostgres`; the payload
    /// follows `DATA_PATH` instead of living in the catalog's database.
    /// NOTE: DuckLake enforces no PRIMARY KEY, so `capture_event`'s
    /// idempotency is enforced in the INSERT statement for this variant.
    AttachedLake { alias: String },
    /// An attached, read-write Postgres table shared by every replica.
    /// `alias` is the DuckDB `ATTACH` alias
    /// (`snapshot::EVENTS_PG_ALIAS`, duplicated here as a plain `String`
    /// rather than a crate cross-reference so `events.rs` has no
    /// dependency on the `snapshot` module — the alias is a SQL
    /// identifier, not shared state).
    AttachedPostgres { alias: String },
}

/// The projection + RFC-3339 readback shared by the list surfaces,
/// against a given fully-qualified table name (the local `events` table
/// or `<alias>.escurel_events`, DuckLake PR 9). `provenance` is cast to
/// `VARCHAR` on read for both backends — the local table stores native
/// `JSON`, the attached-Postgres table stores JSON text in a `VARCHAR`
/// column (see `snapshot::events_pg`'s doc comment) — and `::VARCHAR` is
/// a no-op on an already-`VARCHAR` column.
fn select_cols(table: &str) -> String {
    format!(
        "SELECT event_id, strftime(at_ts, '%Y-%m-%dT%H:%M:%SZ'), \
         source, mime, label_skill, instance_page_id, status, title, body, provenance::VARCHAR \
         FROM {table}"
    )
}

/// Input to [`Indexer::capture_event`]. `event_id` is server-generated
/// (ULID) when `None`; `at` is RFC 3339 (stored as a timestamp);
/// `instance_page_id` may pre-flag a candidate instance (Gmail-label
/// style) while the event still sits in the inbox.
#[derive(Debug, Clone, Default)]
pub struct NewEvent {
    pub event_id: Option<String>,
    pub at: Option<String>,
    pub source: String,
    pub mime: String,
    pub label_skill: String,
    pub instance_page_id: Option<String>,
    pub title: String,
    pub body: String,
    pub provenance: Option<serde_json::Value>,
}

/// One event row, projected for the inbox / event-history surfaces.
#[derive(Debug, Clone, PartialEq)]
pub struct EventInfo {
    pub event_id: String,
    /// RFC 3339 in UTC, or `None` for an undated event.
    pub at: Option<String>,
    pub source: String,
    pub mime: String,
    pub label_skill: String,
    pub instance_page_id: Option<String>,
    pub status: String,
    pub title: String,
    pub body: String,
    pub provenance: serde_json::Value,
}

/// Hard cap on `limit` for the event list surfaces.
pub const EVENTS_MAX_LIMIT: usize = 10_000;

impl Indexer {
    /// The table this indexer's events methods read/write: `events`
    /// (local) or `<alias>.escurel_events` (attached Postgres, DuckLake
    /// PR 9). Mirrors [`Indexer::chat_table`] in `chat.rs` exactly.
    fn events_table(&self) -> String {
        match self.events_backend() {
            EventsBackend::Local => "events".to_owned(),
            EventsBackend::AttachedPostgres { alias } | EventsBackend::AttachedLake { alias } => {
                format!("{alias}.{EVENTS_PG_TABLE_NAME}")
            }
        }
    }

    /// `Some(tenant)` when event rows must be scoped by an explicit
    /// `tenant` column (the attached-Postgres table is one physical
    /// relation shared by every replica of this deployment); `None` for
    /// the local table, whose tenancy is implicit. Mirrors
    /// [`Indexer::chat_tenant_scope`] exactly.
    fn events_tenant_scope(&self) -> Option<&str> {
        match self.events_backend() {
            EventsBackend::Local => None,
            EventsBackend::AttachedPostgres { .. } | EventsBackend::AttachedLake { .. } => {
                Some(self.tenant())
            }
        }
    }

    /// Append one event to the global store; it lands in the inbox
    /// (`status = 'inbox'`). Returns the stored event with its resolved
    /// id + timestamp. A non-null `instance_page_id` is a *candidate*
    /// label only — the event stays in the inbox until `assign_event`.
    ///
    /// **Idempotent on `event_id`.** A caller-supplied `event_id` that
    /// already exists is a no-op (`ON CONFLICT DO NOTHING`) — the existing
    /// stored event is returned unchanged (first-writer-wins), never a
    /// primary-key error. This is what lets the dynamic-workflows reducer
    /// re-emit a content-addressed step id safely (`§3.6`): a re-run or two
    /// racing `reduce` passes collapse to one stored event, and the ledger's
    /// `(tenant, event_id)` unique index collapses the run. The common path
    /// (no `event_id` supplied) mints a fresh ULID and never conflicts.
    ///
    /// No `RETURNING` is involved here — the resolved row is read back
    /// with a separate `SELECT … WHERE event_id = ?` — so this needs no
    /// special-casing for the attached-Postgres table's `RETURNING`
    /// rejection (docs/notes/discovered/
    /// 2026-07-18-duckdb-postgres-attach-no-returning.md); the plain
    /// `INSERT … ON CONFLICT DO NOTHING` + follow-up `SELECT` shape
    /// already avoids it.
    pub async fn capture_event(&self, input: NewEvent) -> Result<EventInfo, IndexerError> {
        let event_id = input
            .event_id
            .clone()
            .unwrap_or_else(|| Ulid::new().to_string());
        let provenance_json = match &input.provenance {
            Some(v) => serde_json::to_string(v)?,
            None => "null".to_owned(),
        };

        let conn = self.conn.lock().await;
        let table = self.events_table();
        // DuckLake enforces no PRIMARY KEY and supports no `ON CONFLICT`,
        // so the lake backend cannot lean on the schema for the
        // first-writer-wins contract above. It gets the same guarantee
        // from an anti-join in the INSERT: the row is only produced when
        // no row with this `event_id` already exists. Same statement, same
        // outcome, enforcement moved from the schema into the query.
        //
        // Note this is a weaker primitive than a PK: two *simultaneous*
        // captures of the same id could both see an empty anti-join and
        // both insert. The runner's own SQLite ledger
        // (`escurel-runner-core/src/ledger.rs`) is what makes
        // exactly-one-run-per-event a hard invariant; this keeps the
        // stored-event side idempotent for the ordinary re-emit case,
        // which is what the dedup contract actually needs.
        let lake = matches!(self.events_backend(), EventsBackend::AttachedLake { .. });
        let sql = match (self.events_tenant_scope(), lake) {
            (None, _) => format!(
                "INSERT INTO {table} \
                 (event_id, at_ts, source, mime, label_skill, instance_page_id, status, title, body, provenance) \
                 VALUES (?, TRY_CAST(? AS TIMESTAMP), ?, ?, ?, ?, 'inbox', ?, ?, ?::JSON) \
                 ON CONFLICT (event_id) DO NOTHING"
            ),
            (Some(_), false) => format!(
                "INSERT INTO {table} \
                 (tenant, event_id, at_ts, source, mime, label_skill, instance_page_id, status, title, body, provenance) \
                 VALUES (?, ?, TRY_CAST(? AS TIMESTAMP), ?, ?, ?, ?, 'inbox', ?, ?, ?) \
                 ON CONFLICT (event_id) DO NOTHING"
            ),
            (Some(_), true) => format!(
                "INSERT INTO {table} \
                 (tenant, event_id, at_ts, source, mime, label_skill, instance_page_id, status, title, body, provenance) \
                 SELECT ?, ?, TRY_CAST(? AS TIMESTAMP), ?, ?, ?, ?, 'inbox', ?, ?, ? \
                 WHERE NOT EXISTS (\
                     SELECT 1 FROM {table} WHERE tenant = ? AND event_id = ?\
                 )"
            ),
        };
        match self.events_tenant_scope() {
            None => {
                conn.execute(
                    &sql,
                    params![
                        event_id,
                        input.at,
                        input.source,
                        input.mime,
                        input.label_skill,
                        input.instance_page_id,
                        input.title,
                        input.body,
                        provenance_json,
                    ],
                )?;
            }
            Some(tenant) if lake => {
                // Two extra binds for the anti-join's WHERE NOT EXISTS.
                conn.execute(
                    &sql,
                    params![
                        tenant,
                        event_id,
                        input.at,
                        input.source,
                        input.mime,
                        input.label_skill,
                        input.instance_page_id,
                        input.title,
                        input.body,
                        provenance_json,
                        tenant,
                        event_id,
                    ],
                )?;
            }
            Some(tenant) => {
                conn.execute(
                    &sql,
                    params![
                        tenant,
                        event_id,
                        input.at,
                        input.source,
                        input.mime,
                        input.label_skill,
                        input.instance_page_id,
                        input.title,
                        input.body,
                        provenance_json,
                    ],
                )?;
            }
        }

        // Read the *stored* row back so a conflicting re-capture returns the
        // authoritative first-writer event (not the discarded second input),
        // and a fresh insert returns exactly what landed.
        // Scope the read-back by tenant on the shared backends. Without
        // it, a caller-supplied `event_id` that collides across tenants
        // reads back another tenant's row — the local table cannot hit
        // this (one DuckDB file per tenant) but a shared Postgres or lake
        // table can.
        let row = match self.events_tenant_scope() {
            None => {
                let select_sql = format!("{} WHERE event_id = ?", select_cols(&table));
                conn.query_row(&select_sql, params![event_id], event_row_from_row)
            }
            Some(tenant) => {
                let select_sql =
                    format!("{} WHERE event_id = ? AND tenant = ?", select_cols(&table));
                conn.query_row(&select_sql, params![event_id, tenant], event_row_from_row)
            }
        }
        .map_err(IndexerError::from)?;
        event_from_row(row)
    }

    /// Unprocessed events (the inbox), newest first.
    pub async fn list_inbox(&self, limit: Option<usize>) -> Result<Vec<EventInfo>, IndexerError> {
        let table = self.events_table();
        let tenant = self.events_tenant_scope().map(str::to_owned);
        let mut where_clauses = vec!["status = 'inbox'".to_owned()];
        if tenant.is_some() {
            where_clauses.push("tenant = ?".to_owned());
        }
        let sql = format!(
            "{} WHERE {} ORDER BY at_ts DESC NULLS LAST, event_id DESC{}",
            select_cols(&table),
            where_clauses.join(" AND "),
            limit_sql(limit),
        );
        self.hydrate_events(&sql, None, tenant.as_deref()).await
    }

    /// An instance's processed event history, oldest first (the event
    /// sequence whose projection is the instance's state).
    pub async fn list_events(
        &self,
        instance_page_id: &str,
        limit: Option<usize>,
    ) -> Result<Vec<EventInfo>, IndexerError> {
        let table = self.events_table();
        let tenant = self.events_tenant_scope().map(str::to_owned);
        let mut where_clauses = vec!["instance_page_id = ? AND status = 'processed'".to_owned()];
        if tenant.is_some() {
            where_clauses.push("tenant = ?".to_owned());
        }
        let sql = format!(
            "{} WHERE {} ORDER BY at_ts ASC NULLS LAST, event_id ASC{}",
            select_cols(&table),
            where_clauses.join(" AND "),
            limit_sql(limit),
        );
        self.hydrate_events(&sql, Some(instance_page_id), tenant.as_deref())
            .await
    }

    /// Assign an inbox event to an instance and mark it processed — the
    /// (external/simulated) agent folding the event into the instance.
    ///
    /// **Compare-and-set on `status`.** The `UPDATE` matches only rows
    /// still in the inbox (`AND status = 'inbox'`), and the rows-affected
    /// count is checked rather than discarded. Before this, two racing
    /// agents could both "succeed" in claiming the same event and the
    /// second silently overwrote the first's `instance_page_id` — the
    /// at-most-once property rested entirely on the runner's separate
    /// SQLite ledger (`escurel-runner-core/src/ledger.rs`), which does not
    /// protect callers that don't go through the runner.
    ///
    /// **Retries stay safe.** A re-run that assigns the same event to the
    /// same instance is `Ok(())`, not an error — `escurel-runner`'s
    /// recovery path re-runs `update_page` + `assign_event` to finish a
    /// partial success (`runner/src/main.rs:1126`), and that must keep
    /// working. Only a *different* instance claiming an already-processed
    /// event is a conflict, and that is exactly the case that used to be
    /// silent data loss.
    pub async fn assign_event(
        &self,
        event_id: &str,
        instance_page_id: &str,
    ) -> Result<(), IndexerError> {
        let conn = self.conn.lock().await;
        let table = self.events_table();
        let updated = match self.events_tenant_scope() {
            None => conn.execute(
                &format!(
                    "UPDATE {table} SET instance_page_id = ?, status = 'processed' \
                     WHERE event_id = ? AND status = 'inbox'"
                ),
                params![instance_page_id, event_id],
            )?,
            Some(tenant) => conn.execute(
                &format!(
                    "UPDATE {table} SET instance_page_id = ?, status = 'processed' \
                     WHERE event_id = ? AND tenant = ? AND status = 'inbox'"
                ),
                params![instance_page_id, event_id, tenant],
            )?,
        };
        if updated > 0 {
            return Ok(());
        }

        // Nothing claimed. Distinguish the reasons rather than reporting a
        // uniform failure: the event may not exist at all, or it may
        // already be processed — either by THIS instance (an idempotent
        // re-run, which must succeed) or by a different one (the real
        // conflict this CAS exists to surface).
        //
        // `QueryReturnedNoRows` is the house idiom for "row absent" here
        // (see `crdt_history::load_snapshot_at`); a row that exists with a
        // NULL `instance_page_id` is a distinct case and must not be
        // collapsed into "missing".
        let row = match self.events_tenant_scope() {
            None => conn.query_row(
                &format!("SELECT instance_page_id FROM {table} WHERE event_id = ?"),
                params![event_id],
                |r| r.get::<_, Option<String>>(0),
            ),
            Some(tenant) => conn.query_row(
                &format!(
                    "SELECT instance_page_id FROM {table} \
                     WHERE event_id = ? AND tenant = ?"
                ),
                params![event_id, tenant],
                |r| r.get::<_, Option<String>>(0),
            ),
        };

        match row {
            Err(duckdb::Error::QueryReturnedNoRows) => Err(IndexerError::EventNotFound {
                event_id: event_id.to_owned(),
            }),
            Err(e) => Err(e.into()),
            // Idempotent re-run: same event, same target instance.
            Ok(Some(existing)) if existing == instance_page_id => Ok(()),
            Ok(Some(existing)) => Err(IndexerError::EventAlreadyAssigned {
                event_id: event_id.to_owned(),
                existing,
                requested: instance_page_id.to_owned(),
            }),
            // Row exists, is not in the inbox, yet carries no instance —
            // a state no normal flow produces (assign always sets both in
            // one statement). Refuse rather than silently binding it, so
            // whatever produced it gets investigated.
            Ok(None) => Err(IndexerError::EventAlreadyAssigned {
                event_id: event_id.to_owned(),
                existing: "<none>".to_owned(),
                requested: instance_page_id.to_owned(),
            }),
        }
    }

    /// Run a `select_cols`-shaped query, filtered by an optional
    /// `instance_page_id` bind (placeholder order: `instance_page_id`
    /// first, then `tenant`, matching [`Self::list_inbox`]'s /
    /// [`Self::list_events`]'s `WHERE` clause construction above) and
    /// hydrate the rows. Only owned `Option<String>`s cross the lock
    /// `.await`, so the future stays `Send` (a non-Send `&[&dyn ToSql]`
    /// would poison the async server handlers).
    async fn hydrate_events(
        &self,
        sql: &str,
        instance: Option<&str>,
        tenant: Option<&str>,
    ) -> Result<Vec<EventInfo>, IndexerError> {
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(sql)?;
        let mut bindings: Vec<Box<dyn duckdb::ToSql + Send>> = Vec::new();
        if let Some(inst) = instance {
            bindings.push(Box::new(inst.to_owned()));
        }
        if let Some(t) = tenant {
            bindings.push(Box::new(t.to_owned()));
        }
        let param_refs: Vec<&dyn duckdb::ToSql> = bindings
            .iter()
            .map(|b| b.as_ref() as &dyn duckdb::ToSql)
            .collect();
        let rows: Vec<EventRow> = stmt
            .query_map(param_refs.as_slice(), event_row_from_row)?
            .collect::<duckdb::Result<Vec<_>>>()?;
        rows.into_iter().map(event_from_row).collect()
    }
}

/// The raw column tuple read from an events row (`SELECT_COLS` order).
type EventRow = (
    String,
    Option<String>,
    String,
    String,
    String,
    Option<String>,
    String,
    String,
    String,
    Option<String>,
);

fn event_row_from_row(row: &duckdb::Row<'_>) -> duckdb::Result<EventRow> {
    Ok((
        row.get(0)?,
        row.get(1)?,
        row.get(2)?,
        row.get(3)?,
        row.get(4)?,
        row.get(5)?,
        row.get(6)?,
        row.get(7)?,
        row.get(8)?,
        row.get(9)?,
    ))
}

fn event_from_row(r: EventRow) -> Result<EventInfo, IndexerError> {
    let (event_id, at, source, mime, label_skill, instance_page_id, status, title, body, prov) = r;
    let provenance = match prov {
        Some(s) => serde_json::from_str(&s)?,
        None => serde_json::Value::Null,
    };
    Ok(EventInfo {
        event_id,
        at,
        source,
        mime,
        label_skill,
        instance_page_id,
        status,
        title,
        body,
        provenance,
    })
}

/// `usize` from our own code, capped so a caller mistake can't OOM us.
fn limit_sql(limit: Option<usize>) -> String {
    limit
        .map(|n| format!(" LIMIT {}", n.min(EVENTS_MAX_LIMIT)))
        .unwrap_or_default()
}
