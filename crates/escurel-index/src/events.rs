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

/// The projection + RFC-3339 readback shared by the list surfaces.
const SELECT_COLS: &str = "SELECT event_id, strftime(at_ts, '%Y-%m-%dT%H:%M:%SZ'), \
     source, mime, label_skill, instance_page_id, status, title, body, provenance::VARCHAR \
     FROM events";

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
    /// Append one event to the global store; it lands in the inbox
    /// (`status = 'inbox'`). Returns the stored event with its resolved
    /// id + timestamp. A non-null `instance_page_id` is a *candidate*
    /// label only — the event stays in the inbox until `assign_event`.
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
        // `at` may be NULL; strftime(NULL) is NULL → Option<String>.
        let stored_at: Option<String> = conn.query_row(
            "INSERT INTO events \
             (event_id, at_ts, source, mime, label_skill, instance_page_id, status, title, body, provenance) \
             VALUES (?, TRY_CAST(? AS TIMESTAMP), ?, ?, ?, ?, 'inbox', ?, ?, ?::JSON) \
             RETURNING strftime(at_ts, '%Y-%m-%dT%H:%M:%SZ')",
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
            |row| row.get(0),
        )?;

        Ok(EventInfo {
            event_id,
            at: stored_at,
            source: input.source,
            mime: input.mime,
            label_skill: input.label_skill,
            instance_page_id: input.instance_page_id,
            status: "inbox".to_owned(),
            title: input.title,
            body: input.body,
            provenance: input.provenance.unwrap_or(serde_json::Value::Null),
        })
    }

    /// Unprocessed events (the inbox), newest first.
    pub async fn list_inbox(&self, limit: Option<usize>) -> Result<Vec<EventInfo>, IndexerError> {
        let sql = format!(
            "{SELECT_COLS} WHERE status = 'inbox' \
             ORDER BY at_ts DESC NULLS LAST, event_id DESC{}",
            limit_sql(limit),
        );
        self.hydrate_events(&sql, None).await
    }

    /// An instance's processed event history, oldest first (the event
    /// sequence whose projection is the instance's state).
    pub async fn list_events(
        &self,
        instance_page_id: &str,
        limit: Option<usize>,
    ) -> Result<Vec<EventInfo>, IndexerError> {
        let sql = format!(
            "{SELECT_COLS} WHERE instance_page_id = ? AND status = 'processed' \
             ORDER BY at_ts ASC NULLS LAST, event_id ASC{}",
            limit_sql(limit),
        );
        self.hydrate_events(&sql, Some(instance_page_id)).await
    }

    /// Assign an inbox event to an instance and mark it processed — the
    /// (external/simulated) agent folding the event into the instance.
    pub async fn assign_event(
        &self,
        event_id: &str,
        instance_page_id: &str,
    ) -> Result<(), IndexerError> {
        let conn = self.conn.lock().await;
        conn.execute(
            "UPDATE events SET instance_page_id = ?, status = 'processed' WHERE event_id = ?",
            params![instance_page_id, event_id],
        )?;
        Ok(())
    }

    /// Run a `SELECT_COLS`-shaped query (optionally filtered by one
    /// `instance_page_id` bind) and hydrate the rows. Only `Option<&str>`
    /// crosses the lock `.await`, so the future stays `Send` (a non-Send
    /// `&[&dyn ToSql]` would poison the async server handlers).
    async fn hydrate_events(
        &self,
        sql: &str,
        instance: Option<&str>,
    ) -> Result<Vec<EventInfo>, IndexerError> {
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(sql)?;
        let rows: Vec<EventRow> = match instance {
            Some(inst) => stmt
                .query_map(params![inst], event_row_from_row)?
                .collect::<duckdb::Result<Vec<_>>>()?,
            None => stmt
                .query_map(params![], event_row_from_row)?
                .collect::<duckdb::Result<Vec<_>>>()?,
        };
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
