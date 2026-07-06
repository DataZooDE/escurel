//! Admin-managed remote-backend endpoint registry (openapi/mcp backends).
//!
//! A skill that declares `backend.kind: openapi` (or `mcp`) references an
//! upstream by **name** (`backend.endpoint: crm_rest`) — never an inline URL
//! (REQ-REMOTE-05). The base URL and any auth secret are registered
//! server-side here, in the `external_endpoints` table of `kb.duckdb`, so the
//! canonical `pages/` corpus stays URL- and secret-free and git-diffable, and
//! `tenant_export` (which tars only `markdown/`) never carries a secret.
//!
//! This is also the **SSRF guard**: a live remote instance can only be
//! pointed at an admin-registered endpoint, so tenant markdown can never make
//! the server fetch an arbitrary host.
//!
//! Like [`crate::creds`] this is a SEPARATE canonical input (REQ-NF-01): not
//! derivable from the corpus, so `rebuild` must not drop it. Mutation is
//! admin-only at the MCP boundary (`escurel-server`); these are the storage
//! primitives behind those tools. [`EndpointInfo`] (the list view) omits the
//! secret; only [`Indexer::lookup_endpoint`] returns it, for the
//! [`crate::RemoteClient`] that must authenticate the outbound call.

use duckdb::params;

use crate::{Indexer, IndexerError};

/// How a registered endpoint authenticates the outbound call. Parsed from the
/// stored `auth_scheme` string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EndpointAuth {
    /// No auth header is sent.
    None,
    /// `Authorization: Bearer <secret>`.
    Bearer,
    /// `<header>: <secret>` (e.g. `X-API-Key: …`).
    ApiKey { header: String },
}

impl EndpointAuth {
    /// The wire string persisted in `external_endpoints.auth_scheme`.
    #[must_use]
    pub fn scheme_str(&self) -> &'static str {
        match self {
            EndpointAuth::None => "none",
            EndpointAuth::Bearer => "bearer",
            EndpointAuth::ApiKey { .. } => "api_key",
        }
    }

    /// Reconstruct from the stored `auth_scheme` + optional `auth_header`.
    #[must_use]
    pub fn from_stored(scheme: &str, header: Option<String>) -> Self {
        match scheme {
            "bearer" => EndpointAuth::Bearer,
            "api_key" => EndpointAuth::ApiKey {
                header: header.unwrap_or_else(|| "X-API-Key".to_owned()),
            },
            _ => EndpointAuth::None,
        }
    }
}

/// One registered endpoint, secret included. Returned only by
/// [`Indexer::lookup_endpoint`] — the [`crate::RemoteClient`] dereferences
/// `backend.endpoint` to this when making the live call. Never serialised
/// onto the agent wire.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EndpointRecord {
    pub name: String,
    /// `openapi` | `mcp`.
    pub kind: String,
    /// Base URL (REST base or MCP `/mcp` URL). Server-side only.
    pub base_url: String,
    pub auth: EndpointAuth,
    /// Bearer token / api-key material. `None` when `auth == None`.
    pub secret: Option<String>,
}

/// Operator-facing view of a registered endpoint — **without** the secret
/// (REQ-REMOTE-05). This is what `list_endpoints` returns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EndpointInfo {
    pub name: String,
    pub kind: String,
    pub base_url: String,
    /// `none` | `bearer` | `api_key`.
    pub auth_scheme: String,
    /// RFC 3339 timestamp the endpoint was registered.
    pub created_at: String,
    /// Admin `sub` who registered it, when recorded.
    pub created_by: Option<String>,
}

impl Indexer {
    /// Register (or replace) a named remote-backend endpoint. Idempotent
    /// upsert on `name`; `created_by` is the admin `sub` performing the
    /// registration, for the audit trail. `kind` is `openapi` | `mcp`.
    pub async fn register_endpoint(
        &self,
        name: &str,
        kind: &str,
        base_url: &str,
        auth: &EndpointAuth,
        secret: Option<&str>,
        created_by: Option<&str>,
    ) -> Result<(), IndexerError> {
        let auth_header = match auth {
            EndpointAuth::ApiKey { header } => Some(header.as_str()),
            _ => None,
        };
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT INTO external_endpoints \
               (name, kind, base_url, auth_scheme, auth_header, secret, created_by) \
             VALUES (?, ?, ?, ?, ?, ?, ?) \
             ON CONFLICT (name) DO UPDATE SET \
               kind = excluded.kind, \
               base_url = excluded.base_url, \
               auth_scheme = excluded.auth_scheme, \
               auth_header = excluded.auth_header, \
               secret = excluded.secret, \
               created_by = excluded.created_by",
            params![
                name,
                kind,
                base_url,
                auth.scheme_str(),
                auth_header,
                secret,
                created_by
            ],
        )?;
        Ok(())
    }

    /// Look up an endpoint by `name`, secret included. `None` when no endpoint
    /// with that name is registered.
    pub async fn lookup_endpoint(
        &self,
        name: &str,
    ) -> Result<Option<EndpointRecord>, IndexerError> {
        let conn = self.conn.lock().await;
        let rec = conn
            .query_row(
                "SELECT name, kind, base_url, auth_scheme, auth_header, secret \
                 FROM external_endpoints WHERE name = ?",
                params![name],
                |r| {
                    let scheme: String = r.get(3)?;
                    let header: Option<String> = r.get(4)?;
                    Ok(EndpointRecord {
                        name: r.get(0)?,
                        kind: r.get(1)?,
                        base_url: r.get(2)?,
                        auth: EndpointAuth::from_stored(&scheme, header),
                        secret: r.get(5)?,
                    })
                },
            )
            .ok();
        Ok(rec)
    }

    /// List every registered endpoint **without** its secret, ordered by
    /// registration time (oldest first).
    pub async fn list_endpoints(&self) -> Result<Vec<EndpointInfo>, IndexerError> {
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(
            "SELECT name, kind, base_url, auth_scheme, created_at::VARCHAR, created_by \
             FROM external_endpoints ORDER BY created_at, name",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(EndpointInfo {
                name: r.get(0)?,
                kind: r.get(1)?,
                base_url: r.get(2)?,
                auth_scheme: r.get(3)?,
                created_at: r.get(4)?,
                created_by: r.get(5)?,
            })
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// Remove a registered endpoint. A no-op when the name is absent.
    pub async fn delete_endpoint(&self, name: &str) -> Result<(), IndexerError> {
        let conn = self.conn.lock().await;
        conn.execute(
            "DELETE FROM external_endpoints WHERE name = ?",
            params![name],
        )?;
        Ok(())
    }
}
