//! Admin-managed external-source credential registry (SQL-view backend).
//!
//! A skill that declares `backend.kind: sql_view` references a credential
//! by **name** (`source.attach: crm_pg`) — never an inline DSN (REQ-SQL-05 /
//! D10). The secret material is registered server-side here, in the
//! `external_credentials` table of `kb.duckdb`, so the canonical `pages/`
//! corpus stays secret-free and git-diffable, and `tenant_export` (which
//! tars only `markdown/`) never carries a secret.
//!
//! This is a SEPARATE canonical input (REQ-NF-01 lists "registered creds"
//! alongside `pages/` + `blobs/`): not derivable from the corpus, so
//! `rebuild` must not drop it. Mutation is admin-only at the MCP boundary
//! (`escurel-server`); these are the storage primitives behind those tools.
//!
//! [`CredentialInfo`] (the list view) deliberately omits the secret so an
//! operator listing never echoes credential material; only
//! [`Indexer::lookup_credential`] returns the secret, for the backend that
//! must build an `ATTACH` / `CREATE SECRET` from it.

use duckdb::params;

use crate::{Indexer, IndexerError};

/// One registered credential, secret included. Returned only by
/// [`Indexer::lookup_credential`] — the backend dereferences `attach` to
/// this when materialising a view. Never serialised onto the agent wire.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CredentialRecord {
    pub name: String,
    pub connector: String,
    pub secret: String,
}

/// Operator-facing view of a registered credential — **without** the secret
/// (REQ-SQL-05). This is what `list_credentials` returns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CredentialInfo {
    pub name: String,
    pub connector: String,
    /// RFC 3339 timestamp the credential was registered.
    pub created_at: String,
    /// Admin `sub` who registered it, when recorded.
    pub created_by: Option<String>,
}

impl Indexer {
    /// Register (or replace) a named external-source credential. Idempotent
    /// upsert on `name`; `created_by` is the admin `sub` performing the
    /// registration, for the audit trail.
    pub async fn register_credential(
        &self,
        name: &str,
        connector: &str,
        secret: &str,
        created_by: Option<&str>,
    ) -> Result<(), IndexerError> {
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT INTO external_credentials (name, connector, secret, created_by) \
             VALUES (?, ?, ?, ?) \
             ON CONFLICT (name) DO UPDATE SET \
               connector = excluded.connector, \
               secret = excluded.secret, \
               created_by = excluded.created_by",
            params![name, connector, secret, created_by],
        )?;
        Ok(())
    }

    /// Look up a credential by `name`, secret included. `None` when no
    /// credential with that name is registered.
    pub async fn lookup_credential(
        &self,
        name: &str,
    ) -> Result<Option<CredentialRecord>, IndexerError> {
        let conn = self.conn.lock().await;
        let rec = conn
            .query_row(
                "SELECT name, connector, secret FROM external_credentials WHERE name = ?",
                params![name],
                |r| {
                    Ok(CredentialRecord {
                        name: r.get(0)?,
                        connector: r.get(1)?,
                        secret: r.get(2)?,
                    })
                },
            )
            .ok();
        Ok(rec)
    }

    /// List every registered credential **without** its secret, ordered by
    /// registration time (oldest first).
    pub async fn list_credentials(&self) -> Result<Vec<CredentialInfo>, IndexerError> {
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(
            "SELECT name, connector, created_at::VARCHAR, created_by \
             FROM external_credentials ORDER BY created_at, name",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(CredentialInfo {
                name: r.get(0)?,
                connector: r.get(1)?,
                created_at: r.get(2)?,
                created_by: r.get(3)?,
            })
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// Remove a registered credential. A no-op when the name is absent.
    pub async fn delete_credential(&self, name: &str) -> Result<(), IndexerError> {
        let conn = self.conn.lock().await;
        conn.execute(
            "DELETE FROM external_credentials WHERE name = ?",
            params![name],
        )?;
        Ok(())
    }
}
