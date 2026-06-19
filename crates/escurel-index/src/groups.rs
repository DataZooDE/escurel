//! DuckDB-canonical custom-group membership (group ACL v1).
//!
//! `group_members` is the source of truth for the membership of groups
//! escurel itself manages (a header may instead name a group that arrives
//! on the JWT, needing no row here). Membership is current-state only —
//! NOT CRDT/time-travelled — with `added_at`/`added_by` audit columns.
//!
//! Mutation is admin-only at the MCP boundary (`escurel-server`); these
//! methods are the storage primitives behind those tools. Reads join into
//! the ACL decision via [`Indexer::duckdb_groups`]; reserved names are
//! stripped where the groups are unioned (`crate::acl`), so a stray
//! reserved-name row can never grant a structural group.

use duckdb::params;

use crate::{Indexer, IndexerError};

/// One `group_members` row, for the operator `list_group_members` view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupMember {
    pub group_id: String,
    pub subject: String,
    /// RFC 3339 timestamp the row was granted.
    pub added_at: String,
    /// Admin `sub` who granted it, when recorded.
    pub added_by: Option<String>,
}

impl Indexer {
    /// The custom groups `subject` belongs to, from `group_members`.
    /// Reserved-name filtering happens at the union site in `crate::acl`,
    /// not here, so this returns rows verbatim. A single indexed lookup on
    /// `group_members_subject`.
    pub async fn duckdb_groups(&self, subject: &str) -> Result<Vec<String>, IndexerError> {
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare("SELECT group_id FROM group_members WHERE subject = ?")?;
        let rows = stmt.query_map(params![subject], |r| r.get::<_, String>(0))?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// Add `subject` to `group_id` (idempotent upsert). `added_by` is the
    /// admin `sub` performing the grant, for the audit trail.
    pub async fn add_group_member(
        &self,
        group_id: &str,
        subject: &str,
        added_by: Option<&str>,
    ) -> Result<(), IndexerError> {
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT INTO group_members (group_id, subject, added_by) VALUES (?, ?, ?) \
             ON CONFLICT (group_id, subject) DO UPDATE SET added_by = excluded.added_by",
            params![group_id, subject, added_by],
        )?;
        Ok(())
    }

    /// Remove `subject` from `group_id`. A no-op when the row is absent.
    pub async fn remove_group_member(
        &self,
        group_id: &str,
        subject: &str,
    ) -> Result<(), IndexerError> {
        let conn = self.conn.lock().await;
        conn.execute(
            "DELETE FROM group_members WHERE group_id = ? AND subject = ?",
            params![group_id, subject],
        )?;
        Ok(())
    }

    /// Every member of `group_id`, ordered by grant time (oldest first).
    pub async fn list_group_members(
        &self,
        group_id: &str,
    ) -> Result<Vec<GroupMember>, IndexerError> {
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(
            "SELECT group_id, subject, added_at::VARCHAR, added_by \
             FROM group_members WHERE group_id = ? ORDER BY added_at, subject",
        )?;
        let rows = stmt.query_map(params![group_id], |r| {
            Ok(GroupMember {
                group_id: r.get(0)?,
                subject: r.get(1)?,
                added_at: r.get(2)?,
                added_by: r.get(3)?,
            })
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }
}
