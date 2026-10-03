//! `migrate_kind`: rewrite a tenant's stored pages from the legacy `type:` page-kind key to
//! `kind:` (stage 1 of the OKF program). Dry run by default.
//!
//! What it touches, and what it deliberately does not:
//!
//! * **Pages** (`markdown/**` in the lane store, and the derived index through `update_page_as`,
//!   which keeps who-wrote-it attribution): rewritten by a text edit of the one top-level key.
//!   A page with BOTH keys is a conflict and is left alone; a user data field named `type:` is
//!   never the page kind; signed pack pages (`markdown/base/**`) are skipped, because rewriting
//!   them breaks the pack signature, and the publisher re-exports a new signed pack instead.
//! * **Open drafts**: rewritten in place. `content_sha256` changes (it is the byte binding a
//!   promotion compares against), so the change is recorded in an audit event. Decided drafts are
//!   history and keep their bytes.
//! * **CRDT snapshots**: every historical snapshot that decodes to a legacy page is re-encoded
//!   from the rewritten markdown, so no reader anywhere has to parse the old key afterwards.
//!   Ops cannot be re-encoded: a page with ops NEWER than its newest snapshot has a live document
//!   that would no longer line up, so `apply` refuses (and names the pages) until the session is
//!   closed or the lanes compacted.
//!
//! The lane store has no conditional write, so there is no true compare-and-swap: each page is
//! re-read immediately before it is written and skipped (reported) if it changed underneath.

use std::collections::HashMap;

use bytes::Bytes;
use duckdb::params;
use escurel_md::{KindRewrite, rewrite_legacy_type_key};
use escurel_storage::Key;
use escurel_types::{DraftKindMigration, MigrateKindReport};

use crate::events::{EventKind, NewEvent};
use crate::indexer::{Indexer, IndexerError, is_archived};

/// Signed pack pages live here; they are never rewritten locally.
const PACK_BASE_PREFIX: &str = "markdown/base/";

/// How many offending pages an error message lists before saying "and N more".
const LISTED: usize = 20;

/// The refusal text: what is wrong, the exact command, and what cannot be migrated locally.
#[must_use]
pub fn legacy_kind_message(tenant: &str, pages: &[String]) -> String {
    let shown: Vec<&str> = pages.iter().take(LISTED).map(String::as_str).collect();
    let more = pages.len().saturating_sub(LISTED);
    let tail = if more > 0 {
        format!(" ... and {more} more")
    } else {
        String::new()
    };
    let base = pages
        .iter()
        .filter(|p| p.starts_with(PACK_BASE_PREFIX))
        .count();
    let pack_note = if base > 0 {
        format!(
            " {base} of them are signed pack pages (markdown/base/...): those cannot be rewritten \
             locally; the pack publisher must re-export and re-sign the pack."
        )
    } else {
        String::new()
    };
    format!(
        "tenant `{tenant}` has {} page(s) that still use the removed `type:` page-kind key (it is \
         `kind:` now): {}{tail}. Run `escurel admin migrate-kind --tenant {tenant}` (a dry run), then \
         with `--apply`.{pack_note}",
        pages.len(),
        shown.join(", ")
    )
}

/// The audit event's label.
pub const KIND_MIGRATION_LABEL: &str = "escurel:kind-migration";

impl Indexer {
    /// Rewrite the tenant's pages, open drafts and historical CRDT snapshots. See the module docs.
    ///
    /// # Errors
    /// [`IndexerError::KindMigrationRefused`] when `apply` is set and a page has a live CRDT
    /// session; otherwise when a store, index or CRDT read/write fails.
    pub async fn migrate_kind(&self, apply: bool) -> Result<MigrateKindReport, IndexerError> {
        let mut report = MigrateKindReport {
            applied: apply,
            ..MigrateKindReport::default()
        };

        report.crdt_pages_with_live_ops = self.pages_with_live_crdt_ops().await?;
        if apply && !report.crdt_pages_with_live_ops.is_empty() {
            return Err(IndexerError::KindMigrationRefused {
                pages: report.crdt_pages_with_live_ops,
            });
        }

        self.migrate_kind_pages(apply, &mut report).await?;
        self.migrate_kind_drafts(apply, &mut report).await?;
        self.migrate_kind_snapshots(apply, &mut report).await?;

        if apply {
            let body = serde_json::to_string(&report)?;
            let event = self
                .capture_event(NewEvent {
                    source: "escurel".to_owned(),
                    mime: "application/json".to_owned(),
                    label_skill: KIND_MIGRATION_LABEL.to_owned(),
                    title: "page kind migrated: type: -> kind:".to_owned(),
                    body,
                    kind: EventKind::System,
                    ..NewEvent::default()
                })
                .await?;
            report.audit_event_id = Some(event.event_id);
        }
        Ok(report)
    }

    /// Every lane page that still carries the removed `type:` page-kind key, sorted. A page that
    /// does not parse for another reason is not listed here: it surfaces as drift or a parse error.
    ///
    /// # Errors
    /// When listing or reading the lane store fails.
    pub async fn legacy_kind_pages(&self) -> Result<Vec<String>, IndexerError> {
        let mut paths: Vec<String> = self.list_markdown_paths().await?.into_iter().collect();
        paths.sort();
        self.legacy_kind_pages_in(&paths).await
    }

    /// [`Self::legacy_kind_pages`] over an already-listed set of lane paths.
    pub(crate) async fn legacy_kind_pages_in(
        &self,
        paths: &[String],
    ) -> Result<Vec<String>, IndexerError> {
        let mut found = Vec::new();
        for path in paths {
            let key = Key::new(self.tenant(), path.clone())?;
            let body = self.store.read(&key).await?;
            let Ok(content) = std::str::from_utf8(&body) else {
                continue;
            };
            if matches!(
                escurel_md::parse(content),
                Err(escurel_md::ParseError::LegacyTypeKey)
            ) {
                found.push(path.clone());
            }
        }
        Ok(found)
    }

    /// Refuse (with [`IndexerError::LegacyKindPages`]) when [`Self::legacy_kind_pages`] is not empty.
    ///
    /// # Errors
    /// [`IndexerError::LegacyKindPages`], or a store failure.
    pub async fn refuse_legacy_kind_pages(&self) -> Result<(), IndexerError> {
        let pages = self.legacy_kind_pages().await?;
        if pages.is_empty() {
            return Ok(());
        }
        Err(IndexerError::LegacyKindPages {
            tenant: self.tenant().to_owned(),
            pages,
        })
    }

    async fn migrate_kind_pages(
        &self,
        apply: bool,
        report: &mut MigrateKindReport,
    ) -> Result<(), IndexerError> {
        let mut paths: Vec<String> = self.list_markdown_paths().await?.into_iter().collect();
        paths.sort();
        report.pages_scanned = paths.len() as u64;
        let attribution = self.written_by_map().await?;

        for path in paths {
            if path.starts_with(PACK_BASE_PREFIX) {
                report.skipped_pack_base.push(path);
                continue;
            }
            let key = Key::new(self.tenant(), path.clone())?;
            let body = self.store.read(&key).await?;
            let Ok(content) = std::str::from_utf8(&body) else {
                report.not_a_page_kind.push(path);
                continue;
            };
            match rewrite_legacy_type_key(content) {
                KindRewrite::AlreadyKind => report.already_kind += 1,
                KindRewrite::Conflict => report.conflicts.push(path),
                KindRewrite::NotAPageKind => report.not_a_page_kind.push(path),
                KindRewrite::Rewritten(new) => {
                    if apply {
                        // No conditional write exists: re-read right before writing and leave the
                        // page alone (it is reported as a conflict) if it moved underneath us.
                        let again = self.store.read(&key).await?;
                        if again[..] != body[..] {
                            report.conflicts.push(path);
                            continue;
                        }
                        if is_archived(content) {
                            // Retained for audit, kept out of the derived index (#300).
                            self.store.write(&key, Bytes::from(new)).await?;
                        } else {
                            self.update_page_as(
                                &path,
                                &new,
                                attribution.get(&path).map(String::as_str),
                            )
                            .await?;
                        }
                    }
                    report.pages_to_migrate.push(path);
                }
            }
        }
        Ok(())
    }

    async fn migrate_kind_drafts(
        &self,
        apply: bool,
        report: &mut MigrateKindReport,
    ) -> Result<(), IndexerError> {
        for draft in self.list_drafts(None).await? {
            let KindRewrite::Rewritten(new) = rewrite_legacy_type_key(&draft.content) else {
                continue;
            };
            let old_sha256 = draft.content_sha256.clone();
            let new_sha256 = crate::drafts::content_hash(&new);
            if apply {
                // `None` = no OPEN draft with that id any more (decided meanwhile): leave it.
                if self
                    .set_draft_content(&draft.draft_id, &new)
                    .await?
                    .is_none()
                {
                    continue;
                }
            }
            report.drafts.push(DraftKindMigration {
                draft_id: draft.draft_id,
                old_sha256,
                new_sha256,
            });
        }
        Ok(())
    }

    async fn migrate_kind_snapshots(
        &self,
        apply: bool,
        report: &mut MigrateKindReport,
    ) -> Result<(), IndexerError> {
        let table = self.crdt_snapshots_table();
        let tenant = self.crdt_tenant_scope().map(str::to_owned);
        let rows: Vec<(String, i64, Vec<u8>)> = {
            let conn = self.conn.lock().await;
            let (sql, scoped) = match &tenant {
                None => (
                    format!("SELECT page_id, snapshot_hlc, snapshot_bytes FROM {table}"),
                    false,
                ),
                Some(_) => (
                    format!(
                        "SELECT page_id, snapshot_hlc, snapshot_bytes FROM {table} WHERE tenant = ?"
                    ),
                    true,
                ),
            };
            let mut stmt = conn.prepare(&sql)?;
            let map = |r: &duckdb::Row<'_>| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, Vec<u8>>(2)?,
                ))
            };
            if scoped {
                stmt.query_map(params![tenant.as_deref().unwrap_or("")], map)?
                    .collect::<Result<Vec<_>, _>>()?
            } else {
                stmt.query_map([], map)?.collect::<Result<Vec<_>, _>>()?
            }
        };

        for (page_id, hlc, bytes) in rows {
            // A snapshot that does not decode is not ours to judge here: history reads surface it.
            let Ok(markdown) = escurel_crdt::body_from_snapshot(&bytes) else {
                continue;
            };
            let KindRewrite::Rewritten(new) = rewrite_legacy_type_key(&markdown) else {
                continue;
            };
            report.snapshots_to_rewrite += 1;
            if !apply {
                continue;
            }
            let new_bytes = escurel_crdt::snapshot_bytes_from_markdown(&new)?;
            let conn = self.conn.lock().await;
            match &tenant {
                None => conn.execute(
                    &format!(
                        "UPDATE {table} SET snapshot_bytes = ? WHERE page_id = ? AND snapshot_hlc = ?"
                    ),
                    params![new_bytes, page_id, hlc],
                )?,
                Some(t) => conn.execute(
                    &format!(
                        "UPDATE {table} SET snapshot_bytes = ? \
                         WHERE page_id = ? AND snapshot_hlc = ? AND tenant = ?"
                    ),
                    params![new_bytes, page_id, hlc, t],
                )?,
            };
            report.snapshots_rewritten += 1;
        }
        Ok(())
    }

    /// Pages with CRDT ops newer than their newest snapshot: a live editing session.
    async fn pages_with_live_crdt_ops(&self) -> Result<Vec<String>, IndexerError> {
        let snapshots = self.crdt_snapshots_table();
        let ops = match self.crdt_pg_backend() {
            crate::indexer::CrdtPgBackend::Local => "crdt_ops".to_owned(),
            crate::indexer::CrdtPgBackend::AttachedPostgres { alias } => {
                format!("{alias}.{}", escurel_crdt::CRDT_OPS_PG_TABLE)
            }
        };
        let tenant = self.crdt_tenant_scope().map(str::to_owned);
        let conn = self.conn.lock().await;
        let (sql, scoped) = match &tenant {
            None => (
                format!(
                    "SELECT DISTINCT o.page_id FROM {ops} o WHERE o.hlc > COALESCE(\
                     (SELECT MAX(s.snapshot_hlc) FROM {snapshots} s WHERE s.page_id = o.page_id), 0) \
                     ORDER BY o.page_id"
                ),
                false,
            ),
            Some(_) => (
                format!(
                    "SELECT DISTINCT o.page_id FROM {ops} o WHERE o.tenant = ? AND o.hlc > COALESCE(\
                     (SELECT MAX(s.snapshot_hlc) FROM {snapshots} s \
                      WHERE s.page_id = o.page_id AND s.tenant = o.tenant), 0) \
                     ORDER BY o.page_id"
                ),
                true,
            ),
        };
        let mut stmt = conn.prepare(&sql)?;
        let pages = if scoped {
            stmt.query_map(params![tenant.as_deref().unwrap_or("")], |r| {
                r.get::<_, String>(0)
            })?
            .collect::<Result<Vec<_>, _>>()?
        } else {
            stmt.query_map([], |r| r.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?
        };
        Ok(pages)
    }

    /// `page_id -> last_written_by`, so a rewrite keeps who wrote each page.
    async fn written_by_map(&self) -> Result<HashMap<String, String>, IndexerError> {
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(
            "SELECT page_id, last_written_by FROM pages WHERE last_written_by IS NOT NULL",
        )?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
        Ok(rows.collect::<Result<HashMap<_, _>, _>>()?)
    }
}
