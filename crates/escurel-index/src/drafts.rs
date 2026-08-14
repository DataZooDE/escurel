//! Draft versions — the pending, unpublished version of a page (CR-2, #354).
//!
//! **A draft is a version whose status is draft.** The version already exists
//! in `crdt_snapshots`; this module records which one is pending and who wrote
//! it, and recovers its content from the snapshot rather than storing a second
//! copy. Two copies of a document that must agree is a bug waiting for a
//! deployment to age.

use duckdb::params;

use crate::{Indexer, IndexerError};

/// A held write, as the store records it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingDraft {
    /// The version this draft IS — `v<hlc>`, the same identifier `expand`
    /// publishes and `base_version` compares.
    pub version: String,
    /// The snapshot holding its content.
    pub snapshot_hlc: i64,
    /// The server-stamped principal that wrote it. `None` only for a row
    /// written before the gateway recorded one.
    ///
    /// This is what makes maker != checker enforceable: **an approval by the
    /// author is not a review.**
    pub drafted_by: Option<String>,
    /// The version it was drafted against, carried so an approval can refuse
    /// a base that has moved.
    pub base_version: Option<String>,
}

impl Indexer {
    /// Record `version` as the pending draft of `page_id`, replacing any
    /// earlier one.
    ///
    /// Replacing rather than queueing is deliberate: competing drafts against
    /// one page are a merge problem, and a reviewer would be choosing between
    /// diffs computed against different bases — the situation
    /// `require_exact_base` exists to refuse. A re-drafting agent means
    /// "instead of", not "as well as".
    pub async fn record_draft(
        &self,
        page_id: &str,
        version: &str,
        snapshot_hlc: i64,
        drafted_by: Option<&str>,
        base_version: Option<&str>,
    ) -> Result<(), IndexerError> {
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT OR REPLACE INTO page_drafts \
             (page_id, version, snapshot_hlc, drafted_by, base_version, created_at) \
             VALUES (?, ?, ?, ?, ?, CURRENT_TIMESTAMP)",
            params![page_id, version, snapshot_hlc, drafted_by, base_version],
        )?;
        Ok(())
    }

    /// The pending draft of `page_id`, if there is one.
    pub async fn pending_draft(&self, page_id: &str) -> Result<Option<PendingDraft>, IndexerError> {
        let conn = self.conn.lock().await;
        let row = conn.query_row(
            "SELECT version, snapshot_hlc, drafted_by, base_version \
             FROM page_drafts WHERE page_id = ?",
            params![page_id],
            |r| {
                Ok(PendingDraft {
                    version: r.get(0)?,
                    snapshot_hlc: r.get(1)?,
                    drafted_by: r.get(2)?,
                    base_version: r.get(3)?,
                })
            },
        );
        match row {
            Ok(d) => Ok(Some(d)),
            Err(duckdb::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Forget the pending draft of `page_id` — it published, or was replaced.
    pub async fn clear_draft(&self, page_id: &str) -> Result<(), IndexerError> {
        let conn = self.conn.lock().await;
        conn.execute(
            "DELETE FROM page_drafts WHERE page_id = ?",
            params![page_id],
        )?;
        Ok(())
    }
}

impl Indexer {
    /// Every page with a pending draft. Unfiltered by skill on purpose: an
    /// unpublished draft has no `pages` row to read a skill from, so the
    /// caller resolves the skill from the draft's own content.
    ///
    /// This is what makes the review queue see a **new** record. Filtering
    /// published instances would show only drafts that edit something that
    /// already exists — and the commonest held write is the one that brings a
    /// record into being, which is precisely the write nobody has looked at.
    pub async fn all_pending_drafts(&self) -> Result<Vec<(String, PendingDraft)>, IndexerError> {
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(
            "SELECT page_id, version, snapshot_hlc, drafted_by, base_version \
             FROM page_drafts ORDER BY created_at",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                PendingDraft {
                    version: r.get(1)?,
                    snapshot_hlc: r.get(2)?,
                    drafted_by: r.get(3)?,
                    base_version: r.get(4)?,
                },
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }
}
