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
//!
//! Every event has a [`EventKind`]: `user` is work (the above); `system`
//! is bookkeeping ABOUT a run, written by the runner or the gateway. A
//! system event never enters the inbox as work — captured with a target
//! page it is stored `processed` on that page at once — and the list
//! surfaces hide it unless asked (`include_system`). `root_event_id` and
//! `run_id` are indexed columns so a lineage is one equality read.

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
/// One page of events plus the resume cursor — the `list_messages`
/// idiom applied to the event surfaces (API review R1). `next_cursor`
/// is `Some` **iff** at least one row lies past the page; its absence —
/// and nothing else — means the listing is complete. A short page is
/// NOT a termination signal: the server-side ACL filter runs after the
/// limit and legitimately shortens pages.
#[derive(Debug, Clone)]
pub struct EventPage {
    pub events: Vec<EventInfo>,
    pub next_cursor: Option<String>,
    /// The cursor of the LAST row on this page — present iff the page is
    /// non-empty, full or not. A tail resumes from it: the next poll returns
    /// exactly what was captured after what this page showed. (`next_cursor`
    /// only says whether more rows already lie past the page.)
    pub resume_cursor: Option<String>,
}

/// Cursor payload: base64url(`"<at-us-or-empty>|<event_id>"`). `at` is
/// the row's `at_ts` at FULL microsecond precision (`%Y-%m-%d
/// %H:%M:%S.%f`), not the second-precision RFC-3339 the wire shows —
/// the equality comparison in the resume predicate must match the
/// stored value exactly or a sub-second row would be replayed. Empty
/// `at` marks `at_ts IS NULL` (the NULLS LAST block).
#[derive(Debug)]
enum EventCursor {
    /// A page's history / the inbox: `(at_ts, event_id)`, chronological.
    At {
        at: Option<String>,
        event_id: String,
    },
    /// A label / lineage / run listing: the ingestion position (hardening
    /// H3), so a backdated event captured after the cursor still follows it.
    Seq(i64),
}

/// The marker that distinguishes a seq cursor from an at cursor inside
/// the opaque string (an `at` never starts with it).
const SEQ_CURSOR_TAG: &str = "seq";

impl EventCursor {
    fn encode_at(scope: &str, at: Option<&str>, event_id: &str) -> String {
        let raw = format!("{}|{}", at.unwrap_or(""), event_id);
        crate::cursor::seal(scope, raw.as_bytes())
    }

    fn encode_seq(scope: &str, seq: i64) -> String {
        crate::cursor::seal(scope, format!("{SEQ_CURSOR_TAG}|{seq}").as_bytes())
    }

    fn decode(scope: &str, raw: &str) -> Result<Self, IndexerError> {
        let bytes = crate::cursor::unseal(scope, raw).ok_or_else(|| {
            IndexerError::InvalidCursor("not a cursor this server issued".to_owned())
        })?;
        let s = std::str::from_utf8(&bytes)
            .map_err(|e| IndexerError::InvalidCursor(format!("utf-8: {e}")))?;
        let (head, tail) = s
            .split_once('|')
            .ok_or_else(|| IndexerError::InvalidCursor("missing separator".to_owned()))?;
        if head == SEQ_CURSOR_TAG {
            let seq = tail
                .parse::<i64>()
                .map_err(|e| IndexerError::InvalidCursor(format!("seq: {e}")))?;
            return Ok(EventCursor::Seq(seq));
        }
        Ok(EventCursor::At {
            at: (!head.is_empty()).then(|| head.to_owned()),
            event_id: tail.to_owned(),
        })
    }
}

fn select_cols(table: &str) -> String {
    // `kind` is COALESCEd: rows written before `0016_events_lineage.sql`
    // carry NULL and are user events by definition.
    format!(
        "SELECT event_id, strftime(at_ts, '%Y-%m-%dT%H:%M:%SZ'), \
         source, mime, label_skill, instance_page_id, status, title, body, provenance::VARCHAR, \
         COALESCE(kind, 'user'), root_event_id, run_id \
         FROM {table}"
    )
}

/// A gateway-minted run whose token lapsed with no terminal (H4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExpiredGatewayRun {
    pub run_id: String,
    pub root_event_id: Option<String>,
    pub instance_page_id: Option<String>,
}

/// The label of a runner's health record.
pub const RUNNER_STATUS_LABEL: &str = "escurel:runner-status";

/// The reserved event-id prefix of a runner's status record
/// (`runner-status:<runner_id>`): one row per runner, overwritten by
/// [`Indexer::upsert_runner_status`]. Only that call (the gateway's
/// `report_runner_status` tool) may mint such an id.
pub const RUNNER_STATUS_ID_PREFIX: &str = "runner-status:";

/// A status record is collected once its runner has been silent for this
/// many of its own reporting intervals, and never sooner than
/// [`RUNNER_STATUS_MIN_WINDOW_S`].
const RUNNER_STATUS_STOPPED_INTERVALS: u64 = 10;
/// The shortest silence after which a runner's record may be collected.
pub const RUNNER_STATUS_MIN_WINDOW_S: u64 = 3600;
/// The interval assumed for a record that does not carry one.
const RUNNER_STATUS_DEFAULT_INTERVAL_S: u64 = 300;

/// What [`Indexer::upsert_runner_status`] stored.
#[derive(Debug, Clone)]
pub struct RunnerStatusUpsert {
    /// The runner's record as stored.
    pub event: EventInfo,
    /// `true` when this call created the record (the runner's first report):
    /// the runner's older `escurel:runner-status` rows were deleted then.
    pub created: bool,
}

/// The label of the audit event a purge leaves behind.
pub const EVENT_PURGE_LABEL: &str = "escurel:event-purge";

/// Labels whose rows are the engine's own audit trail: a purge refuses them
/// unless the caller says so explicitly.
pub const PROTECTED_EVENT_LABELS: [&str; 3] = [
    "escurel:write-back",
    crate::migrate_kind::KIND_MIGRATION_LABEL,
    EVENT_PURGE_LABEL,
];

/// A validated request to purge one label's events (see
/// [`Indexer::purge_events`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PurgeSpec {
    pub label_skill: String,
    /// `YYYY-MM-DD HH:MM:SS[.ffffff]` (UTC): only rows older than this.
    pub before: Option<String>,
    /// Keep the newest N rows of the label whatever their age.
    pub keep_latest: Option<usize>,
}

impl PurgeSpec {
    /// Validate a purge request. A purge must name a label and bound itself
    /// by age, by count, or both (it never means "every row of the label"
    /// by omission); an audit label needs `allow_protected`.
    ///
    /// # Errors
    /// A message for the caller when the request is not acceptable.
    pub fn new(
        label_skill: &str,
        before_rfc3339: Option<&str>,
        keep_latest: Option<usize>,
        allow_protected: bool,
    ) -> Result<Self, String> {
        let label = label_skill.trim();
        if label.is_empty() {
            return Err("label_skill is required".to_owned());
        }
        if before_rfc3339.is_none() && keep_latest.is_none() {
            return Err(
                "give before_ts and/or keep_latest: a purge never deletes a whole label by omission (use keep_latest: 0 to mean exactly that)"
                    .to_owned(),
            );
        }
        if !allow_protected && PROTECTED_EVENT_LABELS.contains(&label) {
            return Err(format!(
                "`{label}` is the engine's own audit trail; pass allow_protected: true to purge it"
            ));
        }
        let before = match before_rfc3339 {
            None => None,
            Some(raw) => {
                let dt = chrono::DateTime::parse_from_rfc3339(raw).map_err(|e| {
                    format!("before_ts must be RFC 3339 (e.g. 2026-10-01T00:00:00Z): {e}")
                })?;
                Some(
                    dt.with_timezone(&chrono::Utc)
                        .format("%Y-%m-%d %H:%M:%S%.6f")
                        .to_string(),
                )
            }
        };
        Ok(Self {
            label_skill: label.to_owned(),
            before,
            keep_latest,
        })
    }
}

/// What a purge counted (and, when applied, deleted).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct PurgeEventsReport {
    pub label_skill: String,
    /// Rows of the label before the purge.
    pub total: u64,
    /// Rows the request selects for deletion.
    pub matched: u64,
    /// Rows that remain (or would remain on a dry run).
    pub kept: u64,
    /// Rows actually deleted: `matched` when applied, `0` on a dry run.
    pub deleted: u64,
    pub dry_run: bool,
    /// The system event a non-empty applied purge leaves behind.
    pub audit_event_id: Option<String>,
}

impl PurgeEventsReport {
    fn new(spec: &PurgeSpec, total: u64, matched: u64, apply: bool) -> Self {
        Self {
            label_skill: spec.label_skill.clone(),
            total,
            matched,
            kept: total.saturating_sub(matched),
            deleted: if apply { matched } else { 0 },
            dry_run: !apply,
            audit_event_id: None,
        }
    }
}

/// The default list surfaces' filter: bookkeeping stays out of a human's
/// (or an agent's) view unless asked for with `include_system`.
const HIDE_SYSTEM: &str = "COALESCE(kind, 'user') <> 'system'";

/// A bound value of an events query. Every backend binds it as a DuckDB
/// parameter; on the attached-Postgres backend the same value can instead
/// be inlined as a Postgres literal, so the whole `WHERE ... ORDER BY ...
/// LIMIT` runs inside Postgres (see [`pg_native`]).
#[derive(Debug, Clone)]
enum Bind {
    Str(String),
    Int(i64),
}

impl duckdb::ToSql for Bind {
    fn to_sql(&self) -> duckdb::Result<duckdb::types::ToSqlOutput<'_>> {
        match self {
            Bind::Str(s) => s.to_sql(),
            Bind::Int(i) => i.to_sql(),
        }
    }
}

/// Native (pushed-down) SQL for the attached-Postgres events table.
///
/// DuckDB's postgres scanner pushes only simple column comparisons into
/// Postgres; `COALESCE(kind, ..)`, `ORDER BY` and `LIMIT` run in DuckDB
/// after the scan, so every listing pulled the whole matching set across
/// the wire (the lab's runner polled the inbox with ~58k hidden heartbeat
/// rows in it, ~120 GiB/day of egress). `postgres_query` / `postgres_execute`
/// run a statement inside Postgres instead; values are inlined as quoted
/// literals, so [`pg_literal`] refuses (and the caller falls back to the
/// scanner path) anything it cannot quote with certainty.
mod pg_native {
    use super::Bind;

    /// `'...'` with `'` doubled; `None` for a value that cannot be quoted
    /// with certainty: a NUL byte, or a backslash (which ends the literal
    /// early under `standard_conforming_strings = off`).
    pub(super) fn pg_literal(s: &str) -> Option<String> {
        if s.contains('\0') || s.contains('\\') {
            return None;
        }
        Some(format!("'{}'", s.replace('\'', "''")))
    }

    impl Bind {
        fn pg_literal(&self) -> Option<String> {
            match self {
                Bind::Str(s) => pg_literal(s),
                Bind::Int(i) => Some(i.to_string()),
            }
        }
    }

    /// `sql` with each `?` replaced, in order, by the matching bind as a
    /// Postgres literal. `None` when a bind cannot be quoted or the counts
    /// disagree (the caller then keeps the DuckDB-bound statement).
    pub(super) fn render(sql: &str, binds: &[Bind]) -> Option<String> {
        let mut out = String::with_capacity(sql.len() + 32);
        let mut next = binds.iter();
        for ch in sql.chars() {
            if ch == '?' {
                out.push_str(&next.next()?.pg_literal()?);
            } else {
                out.push(ch);
            }
        }
        next.next().is_none().then_some(out)
    }

    /// Embed a Postgres statement as a DuckDB string literal argument of
    /// `postgres_query` / `postgres_execute`.
    pub(super) fn duckdb_string(pg_sql: &str) -> String {
        format!("'{}'", pg_sql.replace('\'', "''"))
    }

    /// A cursor timestamp is produced by `strftime(.., '%Y-%m-%d %H:%M:%S.%f')`
    /// and sealed; only that shape is cast natively.
    pub(super) fn is_timestamp_text(s: &str) -> bool {
        !s.is_empty()
            && s.len() <= 40
            && s.chars()
                .all(|c| c.is_ascii_digit() || matches!(c, '-' | ':' | ' ' | '.' | 'T' | 'Z'))
    }
}

/// Whether an event is work (`user`) or bookkeeping about a run (`system`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum EventKind {
    /// Something happened that a skill should fold into an instance — every
    /// event the inbox has ever carried. The default.
    #[default]
    User,
    /// A run's lifecycle, progress, review transitions, runner health
    /// (`escurel:run`, `escurel:review`, …). Skips the inbox; hidden from
    /// the default list surfaces.
    System,
}

impl EventKind {
    /// The stored / wire string.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::System => "system",
        }
    }

    /// `None` for anything but the two known strings.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "user" => Some(Self::User),
            "system" => Some(Self::System),
            _ => None,
        }
    }
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
    /// `user` (the default) or `system`; see [`EventKind`].
    pub kind: EventKind,
    /// The lineage root. `None` ⇒ this event is its own root (the stored
    /// row then carries its own id), which is every user event that is
    /// not a cascade hop.
    pub root_event_id: Option<String>,
    /// The run this system event belongs to; `None` for user events.
    pub run_id: Option<String>,
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
    pub kind: EventKind,
    /// `None` only for a row written before the lineage columns existed.
    pub root_event_id: Option<String>,
    pub run_id: Option<String>,
}

impl EventInfo {
    /// Evolve validation clicks and outcomes are private to the requester's event lineage
    /// even when the general event ACL is still in rollout mode.
    pub fn is_private_evolve_event(&self) -> bool {
        (self.kind == EventKind::User
            && escurel_types::evolve_labels::is_control_label(&self.label_skill))
            || (self.kind == EventKind::System
                && escurel_types::evolve_labels::is_system_label(&self.label_skill)
                && self.instance_page_id.is_none())
    }
}

/// Hard cap on `limit` for the event list surfaces.
pub const EVENTS_MAX_LIMIT: usize = 10_000;

/// What [`Indexer::list_events_filtered_page`] selects. Every `Some`
/// narrows; `include_system` widens (the default hides `system` rows) and
/// an explicit `kind` overrides it. The page and cursor semantics are
/// those of [`Indexer::list_events_page`].
#[derive(Debug, Clone, Default)]
pub struct EventListFilter {
    pub instance_page_id: Option<String>,
    /// The root and everything under it: `root_event_id = ? OR event_id = ?`
    /// (the `OR` covers a root written before the lineage columns existed).
    pub root_event_id: Option<String>,
    pub run_id: Option<String>,
    /// Every event under one label — the runner's subscribers tail a
    /// system label this way (`escurel:review`, `escurel:run-control`).
    pub label_skill: Option<String>,
    pub kind: Option<EventKind>,
    pub include_system: bool,
    /// `inbox` | `processed`; `None` = any status.
    pub status: Option<String>,
}

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

    /// Append one event to the global store. A `user` event lands in the
    /// inbox (`status = 'inbox'`); a non-null `instance_page_id` is then a
    /// *candidate* label only — the event stays in the inbox until
    /// `assign_event`. A `system` event with a target page is stored
    /// `processed` on it at once (bookkeeping is never inbox work); without
    /// one it stays `inbox` but hidden from the default list surfaces.
    /// Returns the stored event with its resolved id + timestamp.
    /// `root_event_id` defaults to the event's own id.
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
        self.capture_event_with_revision(input, None).await
    }

    /// Capture and attest a new Evolve event in one serialized indexer call.
    /// A first-writer collision never gains an attestation.
    pub async fn capture_evolve_event(
        &self,
        input: NewEvent,
        revision_sha256: &str,
    ) -> Result<EventInfo, IndexerError> {
        self.capture_event_with_revision(input, Some(revision_sha256))
            .await
    }

    async fn capture_event_with_revision(
        &self,
        input: NewEvent,
        revision_sha256: Option<&str>,
    ) -> Result<EventInfo, IndexerError> {
        let event_id = input
            .event_id
            .clone()
            .unwrap_or_else(|| Ulid::new().to_string());
        let provenance_json = match &input.provenance {
            Some(v) => serde_json::to_string(v)?,
            None => "null".to_owned(),
        };
        let status = if input.kind == EventKind::System && input.instance_page_id.is_some() {
            "processed"
        } else {
            "inbox"
        };
        let kind = input.kind.as_str();
        let root_event_id = input
            .root_event_id
            .clone()
            .unwrap_or_else(|| event_id.clone());

        let conn = self.conn.lock().await;
        if matches!(self.events_backend(), EventsBackend::Local)
            && self.evolve_revision_column_ready.get().is_none()
        {
            conn.execute_batch(
                "ALTER TABLE events ADD COLUMN IF NOT EXISTS revision_binding_sha256 VARCHAR",
            )?;
            let _ = self.evolve_revision_column_ready.set(());
        }
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
        let native_seq: Option<i64> = match (self.events_backend(), self.events_tenant_scope()) {
            (EventsBackend::AttachedPostgres { alias }, Some(tenant)) => {
                match pg_native::pg_literal(tenant) {
                    Some(lit) => {
                        let inner = format!(
                            "SELECT max(seq) AS m FROM {EVENTS_PG_TABLE_NAME} WHERE tenant = {lit}"
                        );
                        let q = format!(
                            "SELECT COALESCE(m, 0) + 1 FROM postgres_query('{alias}', {})",
                            pg_native::duckdb_string(&inner)
                        );
                        Some(conn.query_row(&q, [], |r| r.get::<_, i64>(0))?)
                    }
                    None => None,
                }
            }
            _ => None,
        };
        let sql = match (self.events_tenant_scope(), lake) {
            (None, _) => format!(
                "INSERT INTO {table} \
                 (event_id, at_ts, source, mime, label_skill, instance_page_id, status, title, body, provenance, \
                  kind, root_event_id, run_id, seq, revision_binding_sha256) \
                 SELECT ?, TRY_CAST(? AS TIMESTAMP), ?, ?, ?, ?, ?, ?, ?, ?::JSON, ?, ?, ?, \
                        COALESCE((SELECT MAX(seq) FROM {table}), 0) + 1, ? \
                 ON CONFLICT (event_id) DO NOTHING"
            ),
            // `created_at` is written EXPLICITLY here, unlike the local
            // table which leans on its `DEFAULT CURRENT_TIMESTAMP`.
            //
            // DuckDB's postgres extension implements INSERT as a COPY: it
            // materialises every column of the target relation and streams
            // the row, so a column omitted from this statement arrives as an
            // explicit NULL rather than being left for Postgres to default.
            // Against `created_at TIMESTAMP NOT NULL DEFAULT now()` that is a
            // not-null violation, and capture_event fails outright — the
            // whole event bus, on the DEFAULT events backend.
            // On the attached-Postgres table the next position is read by a
            // native `max(seq)` (an index-only read) and bound as a value;
            // the scalar subquery form pulled the tenant's whole `seq`
            // column through the scanner on every capture. The read and the
            // insert are still two steps, exactly as the subquery form was
            // (the scanner ran it as a separate scan before the COPY).
            (Some(_), false) if native_seq.is_some() => format!(
                "INSERT INTO {table} \
                 (tenant, event_id, at_ts, source, mime, label_skill, instance_page_id, status, title, body, provenance, created_at, \
                  kind, root_event_id, run_id, seq, revision_binding_sha256) \
                 SELECT ?, ?, TRY_CAST(? AS TIMESTAMP), ?, ?, ?, ?, ?, ?, ?, ?, CURRENT_TIMESTAMP::TIMESTAMP, ?, ?, ?, \
                        CAST(? AS BIGINT), ? \
                 ON CONFLICT (tenant, event_id) DO NOTHING"
            ),
            (Some(_), false) => format!(
                "INSERT INTO {table} \
                 (tenant, event_id, at_ts, source, mime, label_skill, instance_page_id, status, title, body, provenance, created_at, \
                  kind, root_event_id, run_id, seq, revision_binding_sha256) \
                 SELECT ?, ?, TRY_CAST(? AS TIMESTAMP), ?, ?, ?, ?, ?, ?, ?, ?, CURRENT_TIMESTAMP::TIMESTAMP, ?, ?, ?, \
                        COALESCE((SELECT MAX(seq) FROM {table} WHERE tenant = ?), 0) + 1, ? \
                 ON CONFLICT (tenant, event_id) DO NOTHING"
            ),
            (Some(_), true) => format!(
                "INSERT INTO {table} \
                 (tenant, event_id, at_ts, source, mime, label_skill, instance_page_id, status, title, body, provenance, \
                  kind, root_event_id, run_id, seq, revision_binding_sha256) \
                 SELECT ?, ?, TRY_CAST(? AS TIMESTAMP), ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, \
                        COALESCE((SELECT MAX(seq) FROM {table} WHERE tenant = ?), 0) + 1, ? \
                 WHERE NOT EXISTS (\
                     SELECT 1 FROM {table} WHERE tenant = ? AND event_id = ?\
                 )"
            ),
        };
        let inserted = match self.events_tenant_scope() {
            None => conn.execute(
                &sql,
                params![
                    event_id,
                    input.at,
                    input.source,
                    input.mime,
                    input.label_skill,
                    input.instance_page_id,
                    status,
                    input.title,
                    input.body,
                    provenance_json,
                    kind,
                    root_event_id,
                    input.run_id,
                    revision_sha256,
                ],
            )?,
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
                        status,
                        input.title,
                        input.body,
                        provenance_json,
                        kind,
                        root_event_id,
                        input.run_id,
                        // `MAX(seq) WHERE tenant = ?` — the next position.
                        tenant,
                        revision_sha256,
                        tenant,
                        event_id,
                    ],
                )?
            }
            Some(tenant) => {
                // The next position: a bound value (native read above) or,
                // when the tenant could not be quoted, the `MAX(seq)
                // WHERE tenant = ?` subquery's tenant parameter.
                let seq_param: Box<dyn duckdb::ToSql> = match native_seq {
                    Some(n) => Box::new(n),
                    None => Box::new(tenant.to_owned()),
                };
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
                        status,
                        input.title,
                        input.body,
                        provenance_json,
                        kind,
                        root_event_id,
                        input.run_id,
                        seq_param.as_ref(),
                        revision_sha256,
                    ],
                )?
            }
        };
        let _ = inserted; // first-writer collisions preserve the stored proof

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

    /// Server-owned attestation for events captured after this gateway
    /// revision. Historical provenance can never create one retroactively.
    pub async fn evolve_revision_attested(
        &self,
        event_id: &str,
        sha256: &str,
    ) -> Result<bool, IndexerError> {
        let conn = self.conn.lock().await;
        if matches!(self.events_backend(), EventsBackend::Local)
            && self.evolve_revision_column_ready.get().is_none()
        {
            conn.execute_batch(
                "ALTER TABLE events ADD COLUMN IF NOT EXISTS revision_binding_sha256 VARCHAR",
            )?;
            let _ = self.evolve_revision_column_ready.set(());
        }
        let table = self.events_table();
        let tenant = self.events_tenant_scope();
        let sql = if tenant.is_some() {
            format!(
                "SELECT EXISTS (SELECT 1 FROM {table} WHERE tenant = ? AND event_id = ? AND revision_binding_sha256 = ?)"
            )
        } else {
            format!(
                "SELECT EXISTS (SELECT 1 FROM {table} WHERE event_id = ? AND revision_binding_sha256 = ?)"
            )
        };
        Ok(match tenant {
            Some(tenant) => {
                conn.query_row(&sql, params![tenant, event_id, sha256], |row| row.get(0))?
            }
            None => conn.query_row(&sql, params![event_id, sha256], |row| row.get(0))?,
        })
    }

    /// Unprocessed `user` events (the inbox), newest first. System rows
    /// are hidden; [`Self::list_inbox_page`] can include them.
    pub async fn list_inbox(&self, limit: Option<usize>) -> Result<Vec<EventInfo>, IndexerError> {
        // A bounded inbox read on the attached-Postgres backend is the
        // paged listing without a cursor: the same ordering, pushed down.
        if let (EventsBackend::AttachedPostgres { .. }, Some(n)) = (self.events_backend(), limit) {
            if n == 0 {
                return Ok(Vec::new());
            }
            let filter = EventListFilter {
                status: Some("inbox".to_owned()),
                ..Default::default()
            };
            return Ok(self.paged_events(&filter, false, n, None).await?.events);
        }
        let table = self.events_table();
        let tenant = self.events_tenant_scope().map(str::to_owned);
        let mut where_clauses = vec!["status = 'inbox'".to_owned(), HIDE_SYSTEM.to_owned()];
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

    /// An instance's processed `user` event history, oldest first (the
    /// event sequence whose projection is the instance's state). System
    /// rows are hidden; [`Self::list_events_page`] can include them.
    pub async fn list_events(
        &self,
        instance_page_id: &str,
        limit: Option<usize>,
    ) -> Result<Vec<EventInfo>, IndexerError> {
        if let (EventsBackend::AttachedPostgres { .. }, Some(n)) = (self.events_backend(), limit) {
            if n == 0 {
                return Ok(Vec::new());
            }
            let filter = EventListFilter {
                instance_page_id: Some(instance_page_id.to_owned()),
                status: Some("processed".to_owned()),
                ..Default::default()
            };
            return Ok(self.paged_events(&filter, true, n, None).await?.events);
        }
        let table = self.events_table();
        let tenant = self.events_tenant_scope().map(str::to_owned);
        let mut where_clauses = vec![
            "instance_page_id = ? AND status = 'processed'".to_owned(),
            HIDE_SYSTEM.to_owned(),
        ];
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

    /// [`Self::list_inbox`] with a resume cursor (newest first).
    /// `include_system` shows the bookkeeping rows the default hides.
    pub async fn list_inbox_page(
        &self,
        limit: usize,
        cursor: Option<&str>,
        include_system: bool,
    ) -> Result<EventPage, IndexerError> {
        let filter = EventListFilter {
            status: Some("inbox".to_owned()),
            include_system,
            ..Default::default()
        };
        self.paged_events(&filter, false, limit, cursor).await
    }

    /// [`Self::list_events`] with a resume cursor (oldest first). This
    /// is what makes an instance's history past `limit` reachable at
    /// all — without it the tail was permanently silent.
    /// `include_system` shows the bookkeeping rows the default hides.
    pub async fn list_events_page(
        &self,
        instance_page_id: &str,
        limit: usize,
        cursor: Option<&str>,
        include_system: bool,
    ) -> Result<EventPage, IndexerError> {
        let filter = EventListFilter {
            instance_page_id: Some(instance_page_id.to_owned()),
            status: Some("processed".to_owned()),
            include_system,
            ..Default::default()
        };
        self.paged_events(&filter, true, limit, cursor).await
    }

    /// A paged listing over any [`EventListFilter`] — the lineage reads
    /// (`root_event_id`, `run_id`) and the WS resume use this directly.
    /// Same cursor contract as [`Self::list_events_page`].
    pub async fn list_events_filtered_page(
        &self,
        filter: &EventListFilter,
        asc: bool,
        limit: usize,
        cursor: Option<&str>,
    ) -> Result<EventPage, IndexerError> {
        self.paged_events(filter, asc, limit, cursor).await
    }

    /// Shared paged listing over a [`EventListFilter`]. Fetches `limit + 1`
    /// rows to detect a tail; the extra row is dropped and becomes the
    /// cursor.
    ///
    /// The resume predicates mirror `ORDER BY at_ts <dir> NULLS LAST,
    /// event_id <dir>`: a non-NULL cursor row resumes within the
    /// non-NULL block (or crosses into the NULL block, which sorts last
    /// in both directions); a NULL cursor row resumes inside the NULL
    /// block by `event_id` alone (ULIDs are time-ordered).
    async fn paged_events(
        &self,
        filter: &EventListFilter,
        asc: bool,
        limit: usize,
        cursor: Option<&str>,
    ) -> Result<EventPage, IndexerError> {
        let limit = limit.clamp(1, EVENTS_MAX_LIMIT);
        let cursor_scope = format!("events:{}", self.tenant());
        let cursor = match cursor {
            Some(raw) => Some(EventCursor::decode(&cursor_scope, raw)?),
            None => None,
        };
        // A page's history and the inbox are chronological (`at`); a label,
        // a lineage or a run listing is a TAIL and pages by ingestion
        // position, so nothing captured after a cursor can sort before it
        // (hardening H3).
        let by_seq = filter.label_skill.is_some()
            || filter.root_event_id.is_some()
            || filter.run_id.is_some();
        let table = self.events_table();
        let tenant = self.events_tenant_scope().map(str::to_owned);

        // The attached-Postgres table is listed natively: the whole WHERE /
        // ORDER BY / LIMIT runs inside Postgres, so a listing transfers at
        // most `limit + 1` rows however much hidden bookkeeping the table
        // holds. Anything that cannot be quoted with certainty (a backslash
        // or NUL in a filter value) falls back to the scanner path below.
        let alias = match self.events_backend() {
            EventsBackend::AttachedPostgres { alias } => Some(alias),
            _ => None,
        };
        let build = |native: bool| -> (Vec<String>, Vec<Bind>) {
            let mut where_clauses: Vec<String> = Vec::new();
            let mut bindings: Vec<Bind> = Vec::new();
            if let Some(inst) = &filter.instance_page_id {
                where_clauses.push("instance_page_id = ?".to_owned());
                bindings.push(Bind::Str(inst.clone()));
            }
            if let Some(root) = &filter.root_event_id {
                where_clauses.push("(root_event_id = ? OR event_id = ?)".to_owned());
                bindings.push(Bind::Str(root.clone()));
                bindings.push(Bind::Str(root.clone()));
            }
            if let Some(run) = &filter.run_id {
                where_clauses.push("run_id = ?".to_owned());
                bindings.push(Bind::Str(run.clone()));
            }
            if let Some(label) = &filter.label_skill {
                where_clauses.push("label_skill = ?".to_owned());
                bindings.push(Bind::Str(label.clone()));
            }
            if let Some(status) = &filter.status {
                where_clauses.push("status = ?".to_owned());
                bindings.push(Bind::Str(status.clone()));
            }
            match filter.kind {
                Some(kind) => {
                    where_clauses.push("COALESCE(kind, 'user') = ?".to_owned());
                    bindings.push(Bind::Str(kind.as_str().to_owned()));
                }
                None if !filter.include_system => where_clauses.push(HIDE_SYSTEM.to_owned()),
                None => {}
            }
            if where_clauses.is_empty() {
                where_clauses.push("TRUE".to_owned());
            }
            if let Some(t) = &tenant {
                where_clauses.push("tenant = ?".to_owned());
                bindings.push(Bind::Str(t.clone()));
            }
            if let Some(c) = &cursor {
                let cmp = if asc { ">" } else { "<" };
                let ts_cast = if native {
                    "CAST(? AS timestamp)"
                } else {
                    "TRY_CAST(? AS TIMESTAMP)"
                };
                match (by_seq, c) {
                    (true, EventCursor::Seq(seq)) => {
                        where_clauses.push(format!("seq {cmp} ?"));
                        bindings.push(Bind::Int(*seq));
                    }
                    (
                        false,
                        EventCursor::At {
                            at: Some(at),
                            event_id,
                        },
                    ) => {
                        where_clauses.push(format!(
                            "((at_ts IS NOT NULL AND at_ts {cmp} {ts_cast}) \
                             OR (at_ts = {ts_cast} AND event_id {cmp} ?) \
                             OR at_ts IS NULL)"
                        ));
                        bindings.push(Bind::Str(at.clone()));
                        bindings.push(Bind::Str(at.clone()));
                        bindings.push(Bind::Str(event_id.clone()));
                    }
                    (false, EventCursor::At { at: None, event_id }) => {
                        where_clauses.push(format!("(at_ts IS NULL AND event_id {cmp} ?)"));
                        bindings.push(Bind::Str(event_id.clone()));
                    }
                    // A cursor of the other flavour: handled by the caller.
                    _ => {}
                }
            }
            (where_clauses, bindings)
        };
        if let Some(c) = &cursor {
            let mismatched = !matches!(
                (by_seq, c),
                (true, EventCursor::Seq(_)) | (false, EventCursor::At { .. })
            );
            if mismatched {
                // The listing changed shape under the client (or the client
                // mixed selectors).
                return Err(IndexerError::InvalidCursor(
                    "cursor was issued by a listing of another kind".to_owned(),
                ));
            }
        }
        let dir = if asc { "ASC" } else { "DESC" };
        let order = if by_seq {
            format!("seq {dir} NULLS LAST, event_id {dir}")
        } else {
            format!("at_ts {dir} NULLS LAST, event_id {dir}")
        };
        // select_cols + one extra column: the full-precision `at_ts` the
        // NEXT cursor is built from (index 13; the EventRow reader only
        // touches 0..=12, so it is invisible to the wire projection).
        let projection = select_cols(&table);
        let projection = projection
            .strip_suffix(&format!(" FROM {table}"))
            .expect("select_cols ends with FROM <table>");
        let cursor_is_native_safe = match &cursor {
            Some(EventCursor::At { at: Some(at), .. }) => pg_native::is_timestamp_text(at),
            _ => true,
        };
        let native_sql = alias.as_ref().filter(|_| cursor_is_native_safe).and_then(|alias| {
            let (clauses, binds) = build(true);
            let rendered = pg_native::render(&clauses.join(" AND "), &binds)?;
            let inner = format!(
                "SELECT event_id, at_ts, source, mime, label_skill, instance_page_id, status, \
                 title, body, provenance, COALESCE(kind, 'user') AS kind, root_event_id, run_id, seq \
                 FROM {EVENTS_PG_TABLE_NAME} WHERE {rendered} ORDER BY {order} LIMIT {}",
                limit + 1
            );
            Some(format!(
                "SELECT event_id, strftime(at_ts, '%Y-%m-%dT%H:%M:%SZ'), source, mime, label_skill, \
                 instance_page_id, status, title, body, provenance::VARCHAR, kind, root_event_id, run_id, \
                 strftime(at_ts, '%Y-%m-%d %H:%M:%S.%f'), seq \
                 FROM postgres_query('{alias}', {}) ORDER BY {order}",
                pg_native::duckdb_string(&inner)
            ))
        });
        let (sql, bindings) = match native_sql {
            Some(sql) => (sql, Vec::new()),
            None => {
                let (clauses, binds) = build(false);
                (
                    format!(
                        "{projection}, strftime(at_ts, '%Y-%m-%d %H:%M:%S.%f'), seq \
                         FROM {table} WHERE {} \
                         ORDER BY {order} LIMIT {}",
                        clauses.join(" AND "),
                        limit + 1,
                    ),
                    binds,
                )
            }
        };

        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(&sql)?;
        let param_refs: Vec<&dyn duckdb::ToSql> =
            bindings.iter().map(|b| b as &dyn duckdb::ToSql).collect();
        let mut rows: Vec<(EventRow, Option<String>, Option<i64>)> = stmt
            .query_map(param_refs.as_slice(), |row| {
                Ok((
                    event_row_from_row(row)?,
                    row.get::<_, Option<String>>(13)?,
                    row.get::<_, Option<i64>>(14)?,
                ))
            })?
            .collect::<duckdb::Result<Vec<_>>>()?;
        drop(stmt);
        drop(conn);

        let more = rows.len() > limit;
        rows.truncate(limit);
        let resume_cursor = rows.last().map(|(r, at_full, seq)| match (by_seq, seq) {
            (true, Some(seq)) => EventCursor::encode_seq(&cursor_scope, *seq),
            _ => EventCursor::encode_at(&cursor_scope, at_full.as_deref(), &r.0),
        });
        let next_cursor = if more { resume_cursor.clone() } else { None };
        let events = rows
            .into_iter()
            .map(|(r, _, _)| event_from_row(r))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(EventPage {
            events,
            next_cursor,
            resume_cursor,
        })
    }

    /// Keep only the newest `keep` `run-progress` snapshots of one run
    /// (workbench backend FR-P-3): a harness reports its whole plan on every
    /// step change, and `run-finished` carries the final one, so history
    /// past this is redundant for the consumer and unbounded for the store.
    /// Ordered by `at_ts` (the progress writer stamps microseconds) with
    /// `event_id` as the tie-break; tenant-scoped on the shared backends.
    ///
    /// # Errors
    /// When the delete fails.
    pub async fn prune_run_progress(&self, run_id: &str, keep: usize) -> Result<(), IndexerError> {
        let table = self.events_table();
        let tenant = self.events_tenant_scope().map(str::to_owned);
        let scope = if tenant.is_some() {
            " AND tenant = ?"
        } else {
            ""
        };
        let sql = format!(
            "DELETE FROM {table} WHERE run_id = ? AND label_skill = 'escurel:run' \
             AND title = 'run-progress'{scope} AND event_id NOT IN (\
                 SELECT event_id FROM {table} WHERE run_id = ? AND label_skill = 'escurel:run' \
                 AND title = 'run-progress'{scope} \
                 ORDER BY at_ts DESC NULLS LAST, event_id DESC LIMIT {keep})"
        );
        let conn = self.conn.lock().await;
        // Native on the attached-Postgres table, like `prune_label_events`.
        if let (EventsBackend::AttachedPostgres { alias }, Some(t)) =
            (self.events_backend(), &tenant)
            && let (Some(lt), Some(lr)) = (pg_native::pg_literal(t), pg_native::pg_literal(run_id))
        {
            let pg = format!(
                "DELETE FROM {EVENTS_PG_TABLE_NAME} WHERE run_id = {lr} AND label_skill = 'escurel:run' \
                 AND title = 'run-progress' AND tenant = {lt} AND event_id NOT IN (\
                 SELECT event_id FROM {EVENTS_PG_TABLE_NAME} WHERE run_id = {lr} \
                 AND label_skill = 'escurel:run' AND title = 'run-progress' AND tenant = {lt} \
                 ORDER BY at_ts DESC NULLS LAST, event_id DESC LIMIT {keep})"
            );
            conn.execute_batch(&format!(
                "CALL postgres_execute('{alias}', {});",
                pg_native::duckdb_string(&pg)
            ))?;
            return Ok(());
        }
        match &tenant {
            None => conn.execute(&sql, params![run_id, run_id])?,
            Some(t) => conn.execute(&sql, params![run_id, t, run_id, t])?,
        };
        Ok(())
    }

    /// Gateway-minted runs whose token has lapsed with no terminal yet
    /// (hardening H4): `run-started` rows with `provenance.runner.minted_by
    /// = gateway` and an `expires_at` before `now`, for which no
    /// `run-finished` exists. Derived from the events, so a gateway restart
    /// forgets nothing it minted.
    pub async fn expired_gateway_runs(
        &self,
        now_rfc3339: &str,
    ) -> Result<Vec<ExpiredGatewayRun>, IndexerError> {
        let table = self.events_table();
        let tenant = self.events_tenant_scope().map(str::to_owned);
        let scope = if tenant.is_some() {
            " AND s.tenant = ?"
        } else {
            ""
        };
        let sql = format!(
            "SELECT s.run_id, s.root_event_id, s.instance_page_id \
             FROM {table} s \
             WHERE s.label_skill = 'escurel:run' AND s.title = 'run-started' \
               AND json_extract_string(CAST(s.provenance AS VARCHAR), '$.runner.minted_by') = 'gateway' \
               AND json_extract_string(CAST(s.provenance AS VARCHAR), '$.runner.expires_at') < ?{scope} \
               AND s.run_id IS NOT NULL \
               AND NOT EXISTS (\
                   SELECT 1 FROM {table} f WHERE f.run_id = s.run_id AND f.title = 'run-finished'\
               )"
        );
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(&sql)?;
        let map = |row: &duckdb::Row<'_>| {
            Ok(ExpiredGatewayRun {
                run_id: row.get::<_, String>(0)?,
                root_event_id: row.get::<_, Option<String>>(1)?,
                instance_page_id: row.get::<_, Option<String>>(2)?,
            })
        };
        let rows = match &tenant {
            None => stmt.query_map(params![now_rfc3339], map)?,
            Some(t) => stmt.query_map(params![now_rfc3339, t], map)?,
        }
        .collect::<duckdb::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Keep only the newest `keep` rows under `label_skill` for the tenant
    /// (workbench backend P2-4): `escurel:runner-status` is a heartbeat
    /// every N seconds per runner and would otherwise grow without bound.
    /// Same shape as [`Self::prune_run_progress`], keyed by label alone.
    pub async fn prune_label_events(
        &self,
        label_skill: &str,
        keep: usize,
    ) -> Result<(), IndexerError> {
        let table = self.events_table();
        let tenant = self.events_tenant_scope().map(str::to_owned);
        let scope = if tenant.is_some() {
            " AND tenant = ?"
        } else {
            ""
        };
        let sql = format!(
            "DELETE FROM {table} WHERE label_skill = ?{scope} AND event_id NOT IN (\
                 SELECT event_id FROM {table} WHERE label_skill = ?{scope} \
                 ORDER BY at_ts DESC NULLS LAST, event_id DESC LIMIT {keep})"
        );
        let conn = self.conn.lock().await;
        // On the attached-Postgres table the delete runs inside Postgres:
        // through the scanner the `NOT IN (subquery)` pulled every row of
        // the label across the wire first (a 2,000-row backlog read ~60k
        // rows; the lab's was ~58k heartbeats).
        if let (EventsBackend::AttachedPostgres { alias }, Some(t)) =
            (self.events_backend(), &tenant)
            && let (Some(lt), Some(ll)) =
                (pg_native::pg_literal(t), pg_native::pg_literal(label_skill))
        {
            let pg = format!(
                "DELETE FROM {EVENTS_PG_TABLE_NAME} WHERE tenant = {lt} AND label_skill = {ll} \
                 AND event_id NOT IN (SELECT event_id FROM {EVENTS_PG_TABLE_NAME} \
                 WHERE tenant = {lt} AND label_skill = {ll} \
                 ORDER BY at_ts DESC NULLS LAST, event_id DESC LIMIT {keep})"
            );
            conn.execute_batch(&format!(
                "CALL postgres_execute('{alias}', {});",
                pg_native::duckdb_string(&pg)
            ))?;
            return Ok(());
        }
        match &tenant {
            None => conn.execute(&sql, params![label_skill, label_skill])?,
            Some(t) => conn.execute(&sql, params![label_skill, t, label_skill, t])?,
        };
        Ok(())
    }

    /// Purge one label's events (an operator's retention tool): the rows
    /// of `spec.label_skill` older than `spec.before`, except the newest
    /// `spec.keep_latest`. Counts first; deletes only when `apply`. Scoped
    /// to this tenant on the shared backends. On the attached-Postgres
    /// table both the count and the delete run inside Postgres.
    ///
    /// # Errors
    /// When the count or the delete fails.
    pub async fn purge_events(
        &self,
        spec: &PurgeSpec,
        apply: bool,
    ) -> Result<PurgeEventsReport, IndexerError> {
        let mut report = self.purge_events_counted(spec, apply).await?;
        if apply && report.deleted > 0 {
            // The purge is itself an operator action worth a trail: a system
            // event under its own (protected) label, with the report as body.
            let body = serde_json::to_string(&report)?;
            let event = self
                .capture_event(NewEvent {
                    source: "escurel".to_owned(),
                    mime: "application/json".to_owned(),
                    label_skill: EVENT_PURGE_LABEL.to_owned(),
                    title: format!("events purged: {}", report.label_skill),
                    body,
                    kind: EventKind::System,
                    ..NewEvent::default()
                })
                .await?;
            report.audit_event_id = Some(event.event_id);
        }
        Ok(report)
    }

    async fn purge_events_counted(
        &self,
        spec: &PurgeSpec,
        apply: bool,
    ) -> Result<PurgeEventsReport, IndexerError> {
        let table = self.events_table();
        let tenant = self.events_tenant_scope().map(str::to_owned);
        let alias = match self.events_backend() {
            EventsBackend::AttachedPostgres { alias } => Some(alias),
            _ => None,
        };
        // `native` spells the table unqualified (it runs inside Postgres)
        // and casts the cutoff with Postgres' own `CAST`.
        let build = |native: bool| -> (String, String, Vec<Bind>) {
            let tbl = if native {
                EVENTS_PG_TABLE_NAME.to_owned()
            } else {
                table.clone()
            };
            let mut scope = vec!["label_skill = ?".to_owned()];
            let mut binds = vec![Bind::Str(spec.label_skill.clone())];
            if let Some(t) = &tenant {
                scope.push("tenant = ?".to_owned());
                binds.push(Bind::Str(t.clone()));
            }
            let all_of_label = scope.join(" AND ");
            let mut clauses = scope.clone();
            if let Some(before) = &spec.before {
                clauses.push(if native {
                    "at_ts < CAST(? AS timestamp)".to_owned()
                } else {
                    "at_ts < TRY_CAST(? AS TIMESTAMP)".to_owned()
                });
                binds.push(Bind::Str(before.clone()));
            }
            if let Some(n) = spec.keep_latest {
                clauses.push(format!(
                    "event_id NOT IN (SELECT event_id FROM {tbl} WHERE {all_of_label} \
                     ORDER BY at_ts DESC NULLS LAST, event_id DESC LIMIT {n})"
                ));
                binds.push(Bind::Str(spec.label_skill.clone()));
                if let Some(t) = &tenant {
                    binds.push(Bind::Str(t.clone()));
                }
            }
            (all_of_label, clauses.join(" AND "), binds)
        };

        let conn = self.conn.lock().await;
        if let Some(alias) = &alias {
            let (all_sql, match_sql, binds) = build(true);
            let all_binds = &binds[..(if tenant.is_some() { 2 } else { 1 })];
            if let (Some(all_rendered), Some(match_rendered)) = (
                pg_native::render(&all_sql, all_binds),
                pg_native::render(&match_sql, &binds),
            ) {
                let count = |where_sql: &str| -> Result<u64, IndexerError> {
                    let inner =
                        format!("SELECT count(*) FROM {EVENTS_PG_TABLE_NAME} WHERE {where_sql}");
                    let q = format!(
                        "SELECT * FROM postgres_query('{alias}', {})",
                        pg_native::duckdb_string(&inner)
                    );
                    let n: i64 = conn.query_row(&q, [], |r| r.get(0))?;
                    Ok(u64::try_from(n).unwrap_or(0))
                };
                let total = count(&all_rendered)?;
                let matched = count(&match_rendered)?;
                if apply && matched > 0 {
                    let del = format!("DELETE FROM {EVENTS_PG_TABLE_NAME} WHERE {match_rendered}");
                    conn.execute_batch(&format!(
                        "CALL postgres_execute('{alias}', {});",
                        pg_native::duckdb_string(&del)
                    ))?;
                }
                return Ok(PurgeEventsReport::new(spec, total, matched, apply));
            }
        }
        let (all_sql, match_sql, binds) = build(false);
        let all_binds = &binds[..(if tenant.is_some() { 2 } else { 1 })];
        let refs = |b: &[Bind]| -> Vec<Box<dyn duckdb::ToSql>> {
            b.iter()
                .map(|x| Box::new(x.clone()) as Box<dyn duckdb::ToSql>)
                .collect()
        };
        let count = |where_sql: &str, b: &[Bind]| -> Result<u64, IndexerError> {
            let boxed = refs(b);
            let params: Vec<&dyn duckdb::ToSql> = boxed.iter().map(|x| x.as_ref()).collect();
            let n: i64 = conn.query_row(
                &format!("SELECT count(*) FROM {table} WHERE {where_sql}"),
                params.as_slice(),
                |r| r.get(0),
            )?;
            Ok(u64::try_from(n).unwrap_or(0))
        };
        let total = count(&all_sql, all_binds)?;
        let matched = count(&match_sql, &binds)?;
        if apply && matched > 0 {
            let boxed = refs(&binds);
            let params: Vec<&dyn duckdb::ToSql> = boxed.iter().map(|x| x.as_ref()).collect();
            conn.execute(
                &format!("DELETE FROM {table} WHERE {match_sql}"),
                params.as_slice(),
            )?;
        }
        Ok(PurgeEventsReport::new(spec, total, matched, apply))
    }

    /// Overwrite the runner's single status record.
    ///
    /// A runner reports its health every few minutes; as an event stream
    /// that is a table that only grows (the lab's reached ~58k rows and
    /// every poll read them). The record is ONE row per runner under the
    /// reserved id `runner-status:<runner_id>` (`kind: system`, label
    /// [`RUNNER_STATUS_LABEL`]), overwritten in place: `at_ts`, `title` (the
    /// runner's current state), `body` and `provenance` take the new
    /// values. The row sorts by its fresh `at_ts`, so a reader asking for
    /// the label's newest row still finds it first.
    ///
    /// The runner's first report creates the record and, in the same call,
    /// deletes that runner's older heartbeat rows (`provenance.runner_status
    /// .runner_id`), so a backlog written by the stream design is gone
    /// after one report. Updating is UPDATE-then-INSERT (DuckLake has no
    /// primary key to conflict on); on the attached-Postgres table the
    /// whole write runs inside Postgres as one `INSERT … ON CONFLICT DO
    /// UPDATE`. One runner has one writer task, so there is no race to lose.
    ///
    /// # Errors
    /// [`IndexerError::InvalidRunnerStatus`] for a runner id that cannot key
    /// a record; when a statement fails.
    pub async fn upsert_runner_status(
        &self,
        runner_id: &str,
        at: Option<String>,
        title: &str,
        body: &str,
        provenance: &serde_json::Value,
    ) -> Result<RunnerStatusUpsert, IndexerError> {
        if runner_id.is_empty()
            || runner_id.len() > 128
            || !runner_id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | ':' | '@'))
        {
            return Err(IndexerError::InvalidRunnerStatus(format!(
                "runner_id must be 1..=128 characters of [A-Za-z0-9._:@-], got {runner_id:?}"
            )));
        }
        let event_id = format!("{RUNNER_STATUS_ID_PREFIX}{runner_id}");
        let at = at
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(now_rfc3339_micros);
        let provenance_json = serde_json::to_string(provenance)?;

        let mut native = true;
        let created = match self
            .upsert_runner_status_native(runner_id, &event_id, &at, title, body, &provenance_json)
            .await?
        {
            Some(created) => created,
            None => {
                native = false;
                let updated = {
                    let conn = self.conn.lock().await;
                    let table = self.events_table();
                    let json_cast = if matches!(self.events_backend(), EventsBackend::Local) {
                        "?::JSON"
                    } else {
                        "?"
                    };
                    match self.events_tenant_scope() {
                        None => conn.execute(
                            &format!(
                                "UPDATE {table} SET at_ts = TRY_CAST(? AS TIMESTAMP), title = ?, \
                                 body = ?, provenance = {json_cast} WHERE event_id = ?"
                            ),
                            params![at, title, body, provenance_json, event_id],
                        )?,
                        Some(tenant) => conn.execute(
                            &format!(
                                "UPDATE {table} SET at_ts = TRY_CAST(? AS TIMESTAMP), title = ?, \
                                 body = ?, provenance = {json_cast} WHERE event_id = ? AND tenant = ?"
                            ),
                            params![at, title, body, provenance_json, event_id, tenant],
                        )?,
                    }
                };
                if updated > 0 {
                    false
                } else {
                    self.capture_event(NewEvent {
                        event_id: Some(event_id.clone()),
                        at: Some(at.clone()),
                        source: "escurel-runner".to_owned(),
                        mime: "application/json".to_owned(),
                        label_skill: RUNNER_STATUS_LABEL.to_owned(),
                        title: title.to_owned(),
                        body: body.to_owned(),
                        provenance: Some(provenance.clone()),
                        kind: EventKind::System,
                        ..NewEvent::default()
                    })
                    .await?;
                    self.delete_older_runner_status(runner_id, &event_id)
                        .await?;
                    true
                }
            }
        };
        let event = if native {
            // Written inside Postgres: the row is known, so it is not read
            // back through the scanner (that read pulled the whole table).
            EventInfo {
                event_id: event_id.clone(),
                at: Some(format!("{}Z", at[..19.min(at.len())].replace(' ', "T"))),
                source: "escurel-runner".to_owned(),
                mime: "application/json".to_owned(),
                label_skill: RUNNER_STATUS_LABEL.to_owned(),
                instance_page_id: None,
                status: "inbox".to_owned(),
                title: title.to_owned(),
                body: body.to_owned(),
                provenance: provenance.clone(),
                kind: EventKind::System,
                root_event_id: Some(event_id.clone()),
                run_id: None,
            }
        } else {
            self.get_event(&event_id).await?.ok_or_else(|| {
                IndexerError::InvalidRunnerStatus(format!("record {event_id} missing after write"))
            })?
        };
        // A pod that was replaced never reports again, so its record would
        // sit there for good: every report collects the records of runners
        // that have been silent for a clear margin.
        self.collect_stopped_runner_status(&event_id, &at).await?;
        Ok(RunnerStatusUpsert { event, created })
    }

    /// Delete the status records of runners that stopped reporting: a
    /// record whose `at_ts` is older than `max(10 x` its own
    /// `status_interval_s`, [`RUNNER_STATUS_MIN_WINDOW_S`]`)` before `now_at`
    /// (the reporting runner's own report time, so the decision does not
    /// depend on this process' wall clock). Only reserved `runner-status:`
    /// ids of this tenant are candidates, never the record being written,
    /// and never the legacy heartbeat stream (those rows have other ids).
    /// Returns how many were deleted.
    async fn collect_stopped_runner_status(
        &self,
        keep_event_id: &str,
        now_at: &str,
    ) -> Result<usize, IndexerError> {
        let Some(now) = parse_status_time(now_at) else {
            return Ok(0);
        };
        let tenant = self.events_tenant_scope().map(str::to_owned);
        let native = match (self.events_backend(), &tenant) {
            (EventsBackend::AttachedPostgres { alias }, Some(t)) => {
                match (
                    pg_native::pg_literal(t),
                    pg_native::pg_literal(RUNNER_STATUS_LABEL),
                    pg_native::pg_literal(keep_event_id),
                ) {
                    (Some(lt), Some(ll), Some(lk)) => Some((alias, lt, ll, lk)),
                    _ => None,
                }
            }
            _ => None,
        };
        let conn = self.conn.lock().await;
        let candidates: Vec<(String, String, String)> = if let Some((alias, lt, ll, lk)) = &native {
            // The id range (`:` + 1 is `;`) lets the (tenant, event_id) primary key
            // answer it; the label and `LIKE` keep it exact under any collation.
            let inner = format!(
                "SELECT event_id, at_ts::text, body FROM {EVENTS_PG_TABLE_NAME} \
                 WHERE tenant = {lt} AND label_skill = {ll} \
                 AND event_id >= '{RUNNER_STATUS_ID_PREFIX}' AND event_id < 'runner-status;' \
                 AND event_id LIKE '{RUNNER_STATUS_ID_PREFIX}%' AND event_id <> {lk}"
            );
            let mut stmt = conn.prepare(&format!(
                "SELECT event_id, at_ts, body FROM postgres_query('{alias}', {})",
                pg_native::duckdb_string(&inner)
            ))?;
            stmt.query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, Option<String>>(1)?.unwrap_or_default(),
                    r.get::<_, Option<String>>(2)?.unwrap_or_default(),
                ))
            })?
            .collect::<duckdb::Result<Vec<_>>>()?
        } else {
            let table = self.events_table();
            let scope = if tenant.is_some() {
                " AND tenant = ?"
            } else {
                ""
            };
            let sql = format!(
                "SELECT event_id, CAST(at_ts AS VARCHAR), CAST(body AS VARCHAR) FROM {table} \
                 WHERE label_skill = ? AND starts_with(event_id, '{RUNNER_STATUS_ID_PREFIX}') \
                 AND event_id <> ?{scope}"
            );
            let mut stmt = conn.prepare(&sql)?;
            let map = |r: &duckdb::Row<'_>| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, Option<String>>(1)?.unwrap_or_default(),
                    r.get::<_, Option<String>>(2)?.unwrap_or_default(),
                ))
            };
            match &tenant {
                None => stmt.query_map(params![RUNNER_STATUS_LABEL, keep_event_id], map)?,
                Some(t) => stmt.query_map(params![RUNNER_STATUS_LABEL, keep_event_id, t], map)?,
            }
            .collect::<duckdb::Result<Vec<_>>>()?
        };
        let stopped: Vec<String> = candidates
            .into_iter()
            .filter(|(_, at_ts, body)| {
                let Some(last) = parse_status_time(at_ts) else {
                    return false;
                };
                let interval = serde_json::from_str::<serde_json::Value>(body)
                    .ok()
                    .and_then(|v| {
                        v.get("status_interval_s")
                            .and_then(serde_json::Value::as_u64)
                    })
                    .filter(|s| *s > 0)
                    .unwrap_or(RUNNER_STATUS_DEFAULT_INTERVAL_S)
                    .min(86_400);
                let window =
                    (interval * RUNNER_STATUS_STOPPED_INTERVALS).max(RUNNER_STATUS_MIN_WINDOW_S);
                now.signed_duration_since(last).num_seconds() > i64::try_from(window).unwrap_or(0)
            })
            .map(|(id, _, _)| id)
            .collect();
        if stopped.is_empty() {
            return Ok(0);
        }
        if let Some((alias, lt, ll, _)) = &native {
            let ids: Option<Vec<String>> =
                stopped.iter().map(|id| pg_native::pg_literal(id)).collect();
            if let Some(ids) = ids {
                let del = format!(
                    "DELETE FROM {EVENTS_PG_TABLE_NAME} WHERE tenant = {lt} AND label_skill = {ll} \
                     AND event_id IN ({})",
                    ids.join(", ")
                );
                conn.execute_batch(&format!(
                    "CALL postgres_execute('{alias}', {});",
                    pg_native::duckdb_string(&del)
                ))?;
                return Ok(stopped.len());
            }
        }
        let table = self.events_table();
        let marks = vec!["?"; stopped.len()].join(", ");
        let mut binds: Vec<Box<dyn duckdb::ToSql>> = vec![Box::new(RUNNER_STATUS_LABEL)];
        for id in &stopped {
            binds.push(Box::new(id.clone()));
        }
        let scope = if let Some(t) = &tenant {
            binds.push(Box::new(t.clone()));
            " AND tenant = ?"
        } else {
            ""
        };
        let refs: Vec<&dyn duckdb::ToSql> = binds.iter().map(|b| b.as_ref()).collect();
        conn.execute(
            &format!("DELETE FROM {table} WHERE label_skill = ? AND event_id IN ({marks}){scope}"),
            refs.as_slice(),
        )?;
        Ok(stopped.len())
    }

    /// The runner's older heartbeat rows (the stream design's backlog), by
    /// `provenance.runner_status.runner_id`; the record itself is kept.
    async fn delete_older_runner_status(
        &self,
        runner_id: &str,
        keep_event_id: &str,
    ) -> Result<(), IndexerError> {
        let conn = self.conn.lock().await;
        let table = self.events_table();
        match self.events_tenant_scope() {
            None => conn.execute(
                &format!(
                    "DELETE FROM {table} WHERE label_skill = ? AND event_id <> ? \
                     AND json_extract_string(CAST(provenance AS VARCHAR), '$.runner_status.runner_id') = ?"
                ),
                params![RUNNER_STATUS_LABEL, keep_event_id, runner_id],
            )?,
            Some(tenant) => conn.execute(
                &format!(
                    "DELETE FROM {table} WHERE label_skill = ? AND event_id <> ? AND tenant = ? \
                     AND json_extract_string(CAST(provenance AS VARCHAR), '$.runner_status.runner_id') = ?"
                ),
                params![RUNNER_STATUS_LABEL, keep_event_id, tenant, runner_id],
            )?,
        };
        Ok(())
    }

    /// The attached-Postgres write of [`Self::upsert_runner_status`]: one
    /// `INSERT … ON CONFLICT DO UPDATE` and, for a new record, one `DELETE`
    /// of the runner's older rows, both inside Postgres. `Ok(None)` when the
    /// backend is not attached Postgres or a value cannot be quoted with
    /// certainty (the caller then takes the generic path).
    async fn upsert_runner_status_native(
        &self,
        runner_id: &str,
        event_id: &str,
        at: &str,
        title: &str,
        body: &str,
        provenance_json: &str,
    ) -> Result<Option<bool>, IndexerError> {
        let (EventsBackend::AttachedPostgres { alias }, Some(tenant)) =
            (self.events_backend(), self.events_tenant_scope())
        else {
            return Ok(None);
        };
        if !pg_native::is_timestamp_text(at) {
            return Ok(None);
        }
        let lit = |s: &str| pg_native::pg_literal(s);
        let (
            Some(lt),
            Some(lid),
            Some(lat),
            Some(ltitle),
            Some(lbody),
            Some(lprov),
            Some(lrid),
            Some(llabel),
        ) = (
            lit(tenant),
            lit(event_id),
            lit(at),
            lit(title),
            lit(body),
            lit(provenance_json),
            lit(runner_id),
            lit(RUNNER_STATUS_LABEL),
        )
        else {
            return Ok(None);
        };
        let t = EVENTS_PG_TABLE_NAME;
        let conn = self.conn.lock().await;
        let exists_inner = format!("SELECT 1 FROM {t} WHERE tenant = {lt} AND event_id = {lid}");
        let existed: i64 = conn.query_row(
            &format!(
                "SELECT count(*) FROM postgres_query('{alias}', {})",
                pg_native::duckdb_string(&exists_inner)
            ),
            [],
            |r| r.get(0),
        )?;
        let upsert = format!(
            "INSERT INTO {t} (tenant, event_id, at_ts, source, mime, label_skill, status, title, \
                              body, provenance, created_at, kind, seq) \
             VALUES ({lt}, {lid}, CAST({lat} AS timestamp), 'escurel-runner', 'application/json', \
                     {llabel}, 'inbox', {ltitle}, {lbody}, {lprov}, now(), 'system', \
                     (SELECT COALESCE(max(seq), 0) + 1 FROM {t} WHERE tenant = {lt})) \
             ON CONFLICT (tenant, event_id) DO UPDATE SET at_ts = EXCLUDED.at_ts, \
                 title = EXCLUDED.title, body = EXCLUDED.body, provenance = EXCLUDED.provenance"
        );
        conn.execute_batch(&format!(
            "CALL postgres_execute('{alias}', {});",
            pg_native::duckdb_string(&upsert)
        ))?;
        if existed == 0 {
            let del = format!(
                "DELETE FROM {t} WHERE tenant = {lt} AND label_skill = {llabel} \
                 AND event_id <> {lid} \
                 AND provenance::json -> 'runner_status' ->> 'runner_id' = {lrid}"
            );
            conn.execute_batch(&format!(
                "CALL postgres_execute('{alias}', {});",
                pg_native::duckdb_string(&del)
            ))?;
        }
        Ok(Some(existed == 0))
    }

    /// One event by id, whatever its status — the by-event lookup the
    /// instance-scoped surfaces cannot express.
    ///
    /// [`Self::list_inbox`] only sees `status = 'inbox'` and
    /// [`Self::list_events`] needs the instance you are trying to discover,
    /// so an event that has been assigned is reachable only if you already
    /// know where it went. That circularity is what stopped a run with no
    /// pre-flagged target from ever being reconciled: `assign_event` records
    /// the binding, and nothing could read it back.
    ///
    /// Deliberately NOT filtered by status — the caller distinguishes "still
    /// in the inbox" from "assigned to X", and both answers are useful.
    pub async fn get_event(&self, event_id: &str) -> Result<Option<EventInfo>, IndexerError> {
        let table = self.events_table();
        let tenant = self.events_tenant_scope().map(str::to_owned);
        let mut where_clauses = vec!["event_id = ?".to_owned()];
        if tenant.is_some() {
            where_clauses.push("tenant = ?".to_owned());
        }
        let sql = format!(
            "{} WHERE {} LIMIT 1",
            select_cols(&table),
            where_clauses.join(" AND "),
        );
        Ok(self
            .hydrate_events(&sql, Some(event_id), tenant.as_deref())
            .await?
            .into_iter()
            .next())
    }

    /// The **most recent** processed event assigned to `instance` under
    /// `label` — newest by `at_ts` (async-ops Phase-4 F-6). One indexed row, so
    /// deriving an operation's current status is a `LIMIT 1` lookup rather than
    /// paging the whole ASC history (which, capped at [`EVENTS_MAX_LIMIT`],
    /// would report a STALE status for a board with more events than the cap).
    pub async fn latest_labeled_event(
        &self,
        instance: &str,
        label: &str,
    ) -> Result<Option<EventInfo>, IndexerError> {
        let table = self.events_table();
        let tenant = self.events_tenant_scope().map(str::to_owned);
        let mut where_clauses = vec![
            "instance_page_id = ?".to_owned(),
            "label_skill = ?".to_owned(),
            "status = 'processed'".to_owned(),
        ];
        if tenant.is_some() {
            where_clauses.push("tenant = ?".to_owned());
        }
        let sql = format!(
            "{} WHERE {} ORDER BY at_ts DESC NULLS LAST, event_id DESC LIMIT 1",
            select_cols(&table),
            where_clauses.join(" AND "),
        );
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(&sql)?;
        let mut bindings: Vec<Box<dyn duckdb::ToSql + Send>> =
            vec![Box::new(instance.to_owned()), Box::new(label.to_owned())];
        if let Some(t) = &tenant {
            bindings.push(Box::new(t.clone()));
        }
        let param_refs: Vec<&dyn duckdb::ToSql> = bindings
            .iter()
            .map(|b| b.as_ref() as &dyn duckdb::ToSql)
            .collect();
        let row = stmt
            .query_map(param_refs.as_slice(), event_row_from_row)?
            .collect::<duckdb::Result<Vec<_>>>()?
            .into_iter()
            .next();
        row.map(event_from_row).transpose()
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
    String,
    Option<String>,
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
        row.get(10)?,
        row.get(11)?,
        row.get(12)?,
    ))
}

fn event_from_row(r: EventRow) -> Result<EventInfo, IndexerError> {
    let (
        event_id,
        at,
        source,
        mime,
        label_skill,
        instance_page_id,
        status,
        title,
        body,
        prov,
        kind,
        root_event_id,
        run_id,
    ) = r;
    let provenance = match prov {
        Some(s) => serde_json::from_str(&s)?,
        None => serde_json::Value::Null,
    };
    // Anything the store did not write as `system` is a user event —
    // there is no third kind, and refusing to read a row would hide it.
    let kind = EventKind::parse(&kind).unwrap_or(EventKind::User);
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
        kind,
        root_event_id,
        run_id,
    })
}

/// Now, as RFC 3339 UTC with microseconds — an `at` that orders a burst of
/// events captured inside one second (`TRY_CAST` reads the fraction).
#[must_use]
pub fn now_rfc3339_micros() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Micros, true)
}

/// `usize` from our own code, capped so a caller mistake can't OOM us.
fn limit_sql(limit: Option<usize>) -> String {
    limit
        .map(|n| format!(" LIMIT {}", n.min(EVENTS_MAX_LIMIT)))
        .unwrap_or_default()
}

/// A status timestamp as stored (`2026-10-17 08:00:00[.ffffff]`) or reported
/// (RFC 3339), as UTC.
fn parse_status_time(s: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    let s = s.trim();
    chrono::DateTime::parse_from_rfc3339(s)
        .map(|d| d.with_timezone(&chrono::Utc))
        .ok()
        .or_else(|| {
            chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S%.f")
                .ok()
                .map(|n| n.and_utc())
        })
}
