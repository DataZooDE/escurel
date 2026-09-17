//! Branches: an isolated workspace a human later reviews as a unit (#512).
//!
//! escurel already had the READ half — a nullable `scenario` on pages, and
//! `base ∪ overlay` with a deterministic per-slug override — which is a branch
//! *view*. What it had nowhere was the branch as a thing you can name, own,
//! decide and land:
//!
//! - **The registry** ([`BranchInfo`]) gives a branch an author, the corpus
//!   state it forked from, and a status that moves `open → merged |
//!   abandoned` exactly once. Before this, scenarios were discovered by
//!   grepping frontmatter, so "whose workspace is this, and what did it fork
//!   from?" had no answer — and a merge had nothing to compare against.
//! - **Tombstones** ([`Indexer::tombstone_page`]) let an overlay DELETE. An
//!   overlay that can only add or override cannot express "this instance was
//!   wrong, remove it", which is table stakes for a branch.
//! - **The write context** is the dangerous one, and it lives at the server
//!   boundary rather than here: a write names its branch out of band and the
//!   SERVER stamps `scenario`, so an agent working on a branch cannot forget
//!   to stamp a page and write to production instead.
//!
//! The tombstone read rule is where the sharp edge is, and it is recorded in
//! `docs/notes/discovered/2026-05-29-scenario-overlay-qualify.md`: the
//! override picks the overlay row first via `ORDER BY scenario NULLS LAST`,
//! so a winning overlay marked `deleted` must resolve to "not present".
//! Flip that ordering and a delete silently shows the base value again —
//! with no type error anywhere to catch it.

use crate::indexer::{Indexer, IndexerError};

/// A branch, as registered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BranchInfo {
    /// The branch name, e.g. `agent/inbox-scan`. Also the `scenario` value
    /// stamped on its pages, so there is exactly one identifier.
    pub name: String,
    /// The corpus state the branch forked from — what a merge compares
    /// against. Answering that from the branch's own pages would mean
    /// trusting whatever the branch happens to contain.
    pub base_version: String,
    /// The subject that opened it.
    pub author: String,
    /// `open` | `merged` | `abandoned`.
    pub status: String,
    /// Why it was abandoned, when it was.
    pub reason: String,
    /// The subject that merged or abandoned it.
    pub decided_by: String,
    /// RFC 3339.
    pub created_at: String,
}

/// One page's state inside a branch: which slug it overlays, and whether the
/// branch says it should be deleted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BranchPage {
    pub page_id: String,
    pub skill: String,
    pub slug: String,
    /// The branch marked this slug deleted (a tombstone, #512 §3).
    pub deleted: bool,
}

fn row_to_branch(row: &duckdb::Row<'_>) -> duckdb::Result<BranchInfo> {
    Ok(BranchInfo {
        name: row.get(0)?,
        base_version: row.get(1)?,
        author: row.get(2)?,
        status: row.get(3)?,
        reason: row.get(4)?,
        decided_by: row.get(5)?,
        created_at: row.get::<_, Option<String>>(6)?.unwrap_or_default(),
    })
}

const SELECT_BRANCH: &str = "SELECT name, base_version, author, status, reason, decided_by, \
     strftime(created_at, '%Y-%m-%dT%H:%M:%SZ') FROM branches";

impl Indexer {
    /// Register a branch. Returns `Ok(None)` when one of that name already
    /// exists — joining somebody else's workspace by accident is exactly what
    /// a named registry exists to prevent, so the caller reports it rather
    /// than silently reusing it.
    ///
    /// # Errors
    /// When the insert or the read-back fails.
    pub async fn create_branch(
        &self,
        name: &str,
        author: &str,
        base_version: &str,
    ) -> Result<Option<BranchInfo>, IndexerError> {
        {
            let conn = self.conn.lock().await;
            let existing: i64 = conn.query_row(
                "SELECT count(*) FROM branches WHERE name = ?",
                duckdb::params![name],
                |row| row.get(0),
            )?;
            if existing > 0 {
                return Ok(None);
            }
            conn.execute(
                "INSERT INTO branches (name, base_version, author, status, created_at) \
                 VALUES (?, ?, ?, 'open', CURRENT_TIMESTAMP)",
                duckdb::params![name, base_version, author],
            )?;
        }
        // A new isolated workspace is something a subscriber must be able to
        // see, for the same reason a new draft is (#474).
        self.bump_mutation_epoch();
        self.get_branch(name).await
    }

    /// One branch by name, whatever its status.
    ///
    /// # Errors
    /// When the query fails.
    pub async fn get_branch(&self, name: &str) -> Result<Option<BranchInfo>, IndexerError> {
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(&format!("{SELECT_BRANCH} WHERE name = ?"))?;
        let mut rows = stmt.query_map(duckdb::params![name], row_to_branch)?;
        rows.next().transpose().map_err(Into::into)
    }

    /// Every branch, newest first.
    ///
    /// Decided branches are included: "did we already decide that one?" must
    /// stay answerable, which is why the row is kept rather than deleted.
    ///
    /// # Errors
    /// When the query fails.
    pub async fn list_branches(&self) -> Result<Vec<BranchInfo>, IndexerError> {
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(&format!("{SELECT_BRANCH} ORDER BY created_at DESC"))?;
        let rows = stmt.query_map([], row_to_branch)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// Mark a branch decided. `status` is `merged` or `abandoned`.
    ///
    /// Returns `false` when no OPEN branch of that name existed, so a second
    /// decision is a no-op the caller can detect rather than a silent
    /// re-decide.
    ///
    /// # Errors
    /// When the update fails.
    pub async fn close_branch(
        &self,
        name: &str,
        status: &str,
        decided_by: &str,
        reason: &str,
    ) -> Result<bool, IndexerError> {
        let n = {
            let conn = self.conn.lock().await;
            conn.execute(
                "UPDATE branches SET status = ?, decided_by = ?, reason = ?, \
                 decided_at = CURRENT_TIMESTAMP \
                 WHERE name = ? AND status = 'open'",
                duckdb::params![status, decided_by, reason, name],
            )?
        };
        if n > 0 {
            self.bump_mutation_epoch();
        }
        Ok(n > 0)
    }

    /// Every page the branch carries, in slug order.
    ///
    /// This is what a merge iterates: each entry is either an overlay to land
    /// or a tombstone to apply.
    ///
    /// # Errors
    /// When the query fails.
    pub async fn branch_pages(&self, name: &str) -> Result<Vec<BranchPage>, IndexerError> {
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(
            "SELECT page_id, skill, slug, COALESCE(deleted, false) \
             FROM pages WHERE scenario = ? ORDER BY slug",
        )?;
        let rows = stmt.query_map(duckdb::params![name], |row| {
            Ok(BranchPage {
                page_id: row.get(0)?,
                skill: row.get(1)?,
                slug: row.get(2)?,
                deleted: row.get(3)?,
            })
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// Mark an overlay page as a tombstone (#512 §3).
    ///
    /// The row stays — a tombstone IS the branch's statement about the slug,
    /// and deleting the row would make the branch silently fall back to the
    /// base twin instead of hiding it.
    ///
    /// # Errors
    /// When the update fails.
    pub async fn tombstone_page(&self, page_id: &str) -> Result<(), IndexerError> {
        {
            let conn = self.conn.lock().await;
            conn.execute(
                "UPDATE pages SET deleted = true WHERE page_id = ?",
                duckdb::params![page_id],
            )?;
        }
        self.bump_mutation_epoch();
        Ok(())
    }

    /// The `page_id` of the BASE twin of a slug, if the base timeline has one.
    ///
    /// # Errors
    /// When the query fails.
    pub async fn base_twin(&self, skill: &str, slug: &str) -> Result<Option<String>, IndexerError> {
        let conn = self.conn.lock().await;
        Ok(conn
            .query_row(
                "SELECT page_id FROM pages \
                 WHERE skill = ? AND slug = ? AND scenario IS NULL LIMIT 1",
                duckdb::params![skill, slug],
                |row| row.get::<_, String>(0),
            )
            .ok())
    }
}
