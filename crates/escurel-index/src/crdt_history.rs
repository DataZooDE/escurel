//! Historical state via CRDT snapshots (M7).
//!
//! An instance's state over time is the projection of its event
//! sequence. We record that history as real Loro **snapshots** in
//! `crdt_snapshots` (server-authorable, unlike peer-anchored ops), and
//! `expand(as_of = T)` materializes the snapshot at-or-before T — the
//! frontmatter+body *as it was* at that instant.
//!
//! [`Indexer::seed_snapshot_history`] writes a page's snapshot timeline;
//! [`load_snapshot_at`] + [`materialize_snapshot`] are the read side
//! used by [`crate::Indexer::expand`].

use duckdb::params;
use escurel_md::wikilink::parse_wikilinks;

use crate::indexer::{CrdtPgBackend, Indexer, IndexerError};
use crate::read::{BlockInfo, ExpandedPage, PageRef};

impl Indexer {
    /// The table [`Self::list_snapshots`] / [`Self::seed_snapshot_history`]
    /// read/write: `crdt_snapshots` (local) or
    /// `<alias>.escurel_crdt_snapshots` (attached Postgres, DuckLake
    /// PR 10). Mirrors `chat.rs::chat_table` / `events.rs::events_table`.
    pub(crate) fn crdt_snapshots_table(&self) -> String {
        match self.crdt_pg_backend() {
            CrdtPgBackend::Local => "crdt_snapshots".to_owned(),
            CrdtPgBackend::AttachedPostgres { alias } => {
                format!("{alias}.{}", escurel_crdt::CRDT_SNAPSHOTS_PG_TABLE)
            }
        }
    }

    /// `Some(tenant)` when snapshot rows must be scoped by an explicit
    /// `tenant` column (the attached-Postgres table); `None` for the
    /// local table, whose tenancy is implicit. Mirrors
    /// `chat.rs::chat_tenant_scope`.
    pub(crate) fn crdt_tenant_scope(&self) -> Option<&str> {
        match self.crdt_pg_backend() {
            CrdtPgBackend::Local => None,
            CrdtPgBackend::AttachedPostgres { .. } => Some(self.tenant()),
        }
    }

    /// Seed a page's CRDT snapshot history: one snapshot per `(taken_at,
    /// markdown)` state, stamped at the given wall-clock time. This is
    /// how the demo gives an instance a *real* state-over-time history
    /// (each snapshot is a genuine Loro export); `expand(as_of=T)` then
    /// replays it. `taken_at` is RFC 3339; `markdown` is the full page
    /// (frontmatter + body) as it was at that instant.
    pub async fn seed_snapshot_history(
        &self,
        page_id: &str,
        states: &[(&str, &str)],
    ) -> Result<(), IndexerError> {
        let conn = self.conn.lock().await;
        let table = self.crdt_snapshots_table();
        let tenant_scope = self.crdt_tenant_scope();

        // Strictly-increasing hlc above any existing snapshot for the
        // page so the `(page_id, snapshot_hlc)` PK never collides.
        let mut hlc: i64 = match tenant_scope {
            None => conn
                .query_row(
                    &format!(
                        "SELECT COALESCE(MAX(snapshot_hlc), 0) FROM {table} WHERE page_id = ?"
                    ),
                    params![page_id],
                    |r| r.get(0),
                )
                .unwrap_or(0),
            Some(tenant) => conn
                .query_row(
                    &format!(
                        "SELECT COALESCE(MAX(snapshot_hlc), 0) FROM {table} \
                         WHERE page_id = ? AND tenant = ?"
                    ),
                    params![page_id, tenant],
                    |r| r.get(0),
                )
                .unwrap_or(0),
        };
        for (taken_at, markdown) in states {
            let bytes = escurel_crdt::snapshot_bytes_from_markdown(markdown)?;
            hlc += 1;
            match tenant_scope {
                None => {
                    conn.execute(
                        &format!(
                            "INSERT INTO {table} \
                             (page_id, snapshot_hlc, snapshot_bytes, taken_at) \
                             VALUES (?, ?, ?, TRY_CAST(? AS TIMESTAMP))"
                        ),
                        params![page_id, hlc, bytes, taken_at],
                    )?;
                }
                Some(tenant) => {
                    conn.execute(
                        &format!(
                            "INSERT INTO {table} \
                             (tenant, page_id, snapshot_hlc, snapshot_bytes, taken_at) \
                             VALUES (?, ?, ?, ?, TRY_CAST(? AS TIMESTAMP))"
                        ),
                        params![tenant, page_id, hlc, bytes, taken_at],
                    )?;
                }
            }
        }
        Ok(())
    }

    /// The `taken_at` timestamps of a page's snapshot history, oldest
    /// first — the discrete points `expand(as_of = T)` can replay (the
    /// "state over time" version markers in the UI). Empty (not an
    /// error) when the page has no recorded history.
    pub async fn list_snapshots(&self, page_id: &str) -> Result<Vec<String>, IndexerError> {
        let conn = self.conn.lock().await;
        let table = self.crdt_snapshots_table();
        let mut out = Vec::new();
        match self.crdt_tenant_scope() {
            None => {
                let mut stmt = conn.prepare(&format!(
                    "SELECT strftime(taken_at, '%Y-%m-%dT%H:%M:%SZ') FROM {table} \
                     WHERE page_id = ? ORDER BY taken_at ASC, snapshot_hlc ASC"
                ))?;
                let rows = stmt.query_map(params![page_id], |r| r.get::<_, String>(0))?;
                for r in rows {
                    out.push(r?);
                }
            }
            Some(tenant) => {
                let mut stmt = conn.prepare(&format!(
                    "SELECT strftime(taken_at, '%Y-%m-%dT%H:%M:%SZ') FROM {table} \
                     WHERE page_id = ? AND tenant = ? \
                     ORDER BY taken_at ASC, snapshot_hlc ASC"
                ))?;
                let rows = stmt.query_map(params![page_id, tenant], |r| r.get::<_, String>(0))?;
                for r in rows {
                    out.push(r?);
                }
            }
        }
        Ok(out)
    }
}

/// The newest snapshot for `page_id` taken at-or-before `as_of`, or
/// `None` when the page has no snapshot history reaching back that far
/// (the caller then falls through to the current-state path).
///
/// Reads whichever `crdt_snapshots` table the indexer is configured
/// against — the local one, or the shared attached-Postgres one once
/// [`Indexer::attach_crdt_pg`] has run.
///
/// DuckLake PR 10 re-homed `list_snapshots` and the session tools but left
/// this reading the LOCAL table unconditionally. On a reader that is
/// always empty — a reader has no local `crdt_snapshots` rows at all — so
/// `expand(as_of=T)` silently reported "no history" for every page and
/// fell through to current state, which is a wrong answer rather than a
/// missing one. `table` and `tenant` are parameters because this is a free
/// function called with an already-locked connection; the caller resolves
/// them from `crdt_snapshots_table()` / `crdt_tenant_scope()`.
pub(crate) fn load_snapshot_at(
    conn: &duckdb::Connection,
    table: &str,
    tenant: Option<&str>,
    page_id: &str,
    as_of: &str,
) -> Result<Option<Vec<u8>>, IndexerError> {
    let row = match tenant {
        None => conn.query_row(
            &format!(
                "SELECT snapshot_bytes FROM {table} \
                 WHERE page_id = ? AND taken_at <= TRY_CAST(? AS TIMESTAMP) \
                 ORDER BY taken_at DESC, snapshot_hlc DESC LIMIT 1"
            ),
            params![page_id, as_of],
            |r| r.get::<_, Vec<u8>>(0),
        ),
        Some(tenant) => conn.query_row(
            &format!(
                "SELECT snapshot_bytes FROM {table} \
                 WHERE page_id = ? AND tenant = ? \
                 AND taken_at <= TRY_CAST(? AS TIMESTAMP) \
                 ORDER BY taken_at DESC, snapshot_hlc DESC LIMIT 1"
            ),
            params![page_id, tenant, as_of],
            |r| r.get::<_, Vec<u8>>(0),
        ),
    };
    match row {
        Ok(bytes) => Ok(Some(bytes)),
        Err(duckdb::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Decode a Loro snapshot blob and re-parse it into an [`ExpandedPage`]
/// — the instance materialized at that historical point.
pub(crate) fn materialize_snapshot(
    page_id: &str,
    snapshot: &[u8],
) -> Result<ExpandedPage, IndexerError> {
    let markdown = escurel_crdt::body_from_snapshot(snapshot)?;
    let parsed = escurel_md::parse(&markdown)?;
    let fields = &parsed.frontmatter.fields;

    let frontmatter = serde_json::to_value(fields)?;
    let skill = fields
        .get("skill")
        .and_then(escurel_md::YamlValue::as_str)
        .or_else(|| fields.get("id").and_then(escurel_md::YamlValue::as_str))
        .unwrap_or("")
        .to_owned();
    let slug = fields
        .get("id")
        .and_then(escurel_md::YamlValue::as_str)
        .map(str::to_owned);
    let page_type = parsed.frontmatter.page_type;

    let body = parsed.body.to_owned();
    let wikilinks_out = parse_wikilinks(&body);
    let blocks = vec![BlockInfo {
        anchor: "blk-0".to_owned(),
        content: body.clone(),
    }];

    Ok(ExpandedPage {
        page: PageRef {
            page_id: page_id.to_owned(),
            slug,
            skill,
            page_type,
        },
        frontmatter,
        body,
        blocks,
        wikilinks_out,
    })
}
