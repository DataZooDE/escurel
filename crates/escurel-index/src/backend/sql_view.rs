//! Read-only SQL-view backend (REQ-SQL-*).
//!
//! Creating a `sql_view` instance **materialises a read-only DuckDB VIEW**
//! over an external source — postgres/mysql/sqlite/erpl scanners, or a
//! directory of JSON/Parquet files — and writes a markdown **overlay page**
//! carrying a `backend_ref` block that binds the instance to the view
//! (HLD §5, D2/D3). Reading the instance reads the view (PR-2c).
//!
//! ## Invariants encoded here
//!
//! - **Read-only (N1, REQ-SQL-04).** Database connectors `ATTACH … (TYPE …,
//!   READ_ONLY)`, which makes writes engine-rejected; directory connectors
//!   use the `read_json_auto` / `read_parquet` table functions, which are
//!   inherently read-only. No DDL/DML is ever issued against the external
//!   system.
//! - **Secrets never in markdown (REQ-SQL-05).** A DB connector dereferences
//!   its DSN from the admin credential registry ([`Indexer::lookup_credential`])
//!   by the skill's `source.attach` name; the overlay page records only the
//!   view name + hashes, never the secret.
//! - **Built over the existing attach plumbing (REQ-SQL-03).** The READ_ONLY
//!   `ATTACH` is the same native mechanism `attach_external` uses; this is
//!   the first-class promotion of the origin axis, not a parallel one.
//!
//! Live postgres/mysql/sqlite/ERPL connectivity needs an external system (or
//! the scanner extension installed) and is the documented residual; the
//! offline tests cover the directory connectors, the fail-closed
//! missing-credential path, and the read-only `ATTACH` builder.

use std::sync::Arc;

use sha2::{Digest, Sha256};

use super::binding::{SqlConnector, SqlViewBinding};
use crate::Indexer;

/// The result of materialising a SQL-view instance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Materialized {
    /// The overlay page id (the canonical markdown key).
    pub page_id: String,
    /// The deterministic DuckDB view name.
    pub view: String,
    /// Hash of the binding (connector/relation/filter/project/search_text).
    pub binding_hash: String,
    /// Hash of the view's result schema, captured at create time
    /// (REQ-NF-06 schema-drift detection compares against this).
    pub source_schema_fingerprint: String,
}

/// Failure modes of SQL-view materialisation. Each maps to a typed `Issue`
/// at the dispatcher boundary (PR-2c).
#[derive(Debug, thiserror::Error)]
pub enum SqlViewError {
    #[error("backend_unavailable: credential `{0}` is not registered")]
    CredentialNotFound(String),
    #[error("backend_unavailable: connector `{0}` requires a `source.attach` credential")]
    MissingAttach(&'static str),
    #[error(
        "backend_unavailable: the `erpl` connector is unsigned; the tenant connection must be \
         opened with allow_unsigned_extensions (admin opt-in)"
    )]
    UnsignedExtensionNotAllowed,
    #[error("invalid sql-view binding: {0}")]
    InvalidBinding(String),
    #[error("duckdb error: {0}")]
    Duckdb(#[from] duckdb::Error),
    #[error(transparent)]
    Indexer(#[from] crate::IndexerError),
}

/// Read-only SQL-view backend. Wraps the shared [`Indexer`] (its DuckDB
/// connection hosts the views + the credential registry).
pub struct SqlViewBackend {
    indexer: Arc<Indexer>,
    /// Admin opt-in to load the unsigned ERPL/SAP community extension
    /// (widened trust boundary; spike S1). Off by default.
    allow_unsigned_extensions: bool,
}

impl SqlViewBackend {
    #[must_use]
    pub fn new(indexer: Arc<Indexer>) -> Self {
        Self {
            indexer,
            allow_unsigned_extensions: false,
        }
    }

    /// Enable loading the unsigned ERPL community extension (SAP). Gate this
    /// behind an explicit admin opt-in — it widens the connection's trust
    /// boundary (spike S1).
    #[must_use]
    pub fn with_unsigned_extensions(mut self, allow: bool) -> Self {
        self.allow_unsigned_extensions = allow;
        self
    }

    /// Materialise a read-only view for one instance and write its overlay
    /// page. Runs the view DDL on the indexer connection, captures the
    /// schema fingerprint, then writes the overlay via the normal
    /// `update_page` path (which takes the per-tenant write lock).
    pub async fn create_instance(
        &self,
        skill: &str,
        binding: &SqlViewBinding,
        instance_id: &str,
        overlay_body: &str,
    ) -> Result<Materialized, SqlViewError> {
        let view = view_name(skill, instance_id);
        let source_expr = self.prepare_source(binding).await?;

        let filter_sql = match binding.filter.as_deref() {
            Some(f) if !f.trim().is_empty() => {
                if !is_safe_sql_fragment(f) {
                    return Err(SqlViewError::InvalidBinding(format!(
                        "filter contains an unsafe character: {f:?}"
                    )));
                }
                format!(" WHERE {f}")
            }
            _ => String::new(),
        };

        let fingerprint = {
            let conn = self.indexer.conn.lock().await;
            conn.execute_batch(&format!(
                "CREATE OR REPLACE VIEW {view} AS SELECT * FROM {source_expr}{filter_sql}"
            ))?;
            schema_fingerprint(&conn, &view)?
        };

        let binding_hash = hash_binding(binding);
        let page_id = format!("markdown/instances/{skill}/{instance_id}.md");
        let content = overlay_markdown(
            skill,
            instance_id,
            &view,
            &binding_hash,
            &fingerprint,
            overlay_body,
        );
        self.indexer.update_page(&page_id, &content).await?;

        Ok(Materialized {
            page_id,
            view,
            binding_hash,
            source_schema_fingerprint: fingerprint,
        })
    }

    /// Read up to `limit` rows from a materialised view as JSON objects.
    /// Used by the read path (PR-2c) to render a bounded projection.
    pub async fn project_rows(
        &self,
        view: &str,
        limit: usize,
    ) -> Result<Vec<serde_json::Map<String, serde_json::Value>>, SqlViewError> {
        let conn = self.indexer.conn.lock().await;
        project_view_rows(&conn, view, limit)
    }

    /// Resolve the FROM-clause source expression, performing any required
    /// INSTALL/LOAD + READ_ONLY ATTACH first. Directory connectors need no
    /// credential; DB connectors dereference the admin credential registry.
    async fn prepare_source(&self, binding: &SqlViewBinding) -> Result<String, SqlViewError> {
        match binding.connector {
            SqlConnector::JsonDir | SqlConnector::ParquetDir => {
                if !is_safe_sql_fragment(&binding.relation) {
                    return Err(SqlViewError::InvalidBinding(format!(
                        "relation/glob contains an unsafe character: {:?}",
                        binding.relation
                    )));
                }
                let glob = directory_glob(binding.connector, &binding.relation);
                let func = match binding.connector {
                    SqlConnector::JsonDir => "read_json_auto",
                    SqlConnector::ParquetDir => "read_parquet",
                    _ => unreachable!(),
                };
                Ok(format!("{func}('{glob}')"))
            }
            db => {
                let attach = binding
                    .attach
                    .as_deref()
                    .ok_or(SqlViewError::MissingAttach(db.as_str()))?;
                let cred = self
                    .indexer
                    .lookup_credential(attach)
                    .await?
                    .ok_or_else(|| SqlViewError::CredentialNotFound(attach.to_owned()))?;
                if matches!(db, SqlConnector::Erpl) && !self.allow_unsigned_extensions {
                    return Err(SqlViewError::UnsignedExtensionNotAllowed);
                }
                if !is_safe_sql_fragment(&cred.secret) {
                    return Err(SqlViewError::InvalidBinding(
                        "registered secret contains an unsafe character".to_owned(),
                    ));
                }
                if !is_safe_sql_fragment(&binding.relation) || !is_valid_identifier(attach) {
                    return Err(SqlViewError::InvalidBinding(
                        "relation or attach name is unsafe".to_owned(),
                    ));
                }
                let conn = self.indexer.conn.lock().await;
                if matches!(db, SqlConnector::Erpl) && self.allow_unsigned_extensions {
                    conn.execute_batch("SET allow_unsigned_extensions=true;")?;
                }
                for stmt in install_load(db) {
                    conn.execute_batch(stmt)?;
                }
                conn.execute_batch(&attach_sql(db, attach, &cred.secret))?;
                Ok(format!("{attach}.{}", binding.relation))
            }
        }
    }
}

/// Read up to `limit` rows from a materialised view as JSON objects, on an
/// already-locked connection. Shared by [`SqlViewBackend::project_rows`] and
/// [`crate::Indexer::project_view`] (the read-path projection, PR-2c).
pub(crate) fn project_view_rows(
    conn: &duckdb::Connection,
    view: &str,
    limit: usize,
) -> Result<Vec<serde_json::Map<String, serde_json::Value>>, SqlViewError> {
    if !is_valid_identifier(view) {
        return Err(SqlViewError::InvalidBinding(format!(
            "not a valid view identifier: {view:?}"
        )));
    }
    // Column names come from DESCRIBE (the duckdb-rs `Statement::column_names`
    // schema is only populated after stepping; deriving names up front keeps
    // the row loop simple and index-aligned).
    let cols: Vec<String> = describe(conn, view)?.into_iter().map(|(n, _t)| n).collect();
    let sql = format!("SELECT * FROM {view} LIMIT {}", limit.min(10_000));
    let mut stmt = conn.prepare(&sql)?;
    let mut rows = stmt.query([])?;
    let mut out = Vec::new();
    while let Some(row) = rows.next()? {
        let mut obj = serde_json::Map::new();
        for (i, name) in cols.iter().enumerate() {
            obj.insert(name.clone(), duck_value_to_json(row, i));
        }
        out.push(obj);
    }
    Ok(out)
}

/// Build the READ_ONLY `ATTACH` for a database connector. Pure so the
/// no-write-back invariant (REQ-SQL-04) is unit-testable without a live
/// source. `alias` and `secret` are validated by the caller.
#[must_use]
pub fn attach_sql(connector: SqlConnector, alias: &str, secret: &str) -> String {
    let ty = match connector {
        SqlConnector::Postgres => "postgres",
        SqlConnector::Mysql => "mysql",
        SqlConnector::Sqlite => "sqlite",
        SqlConnector::Erpl => "erpl",
        SqlConnector::JsonDir | SqlConnector::ParquetDir => "",
    };
    format!("ATTACH '{secret}' AS {alias} (TYPE {ty}, READ_ONLY)")
}

/// The INSTALL/LOAD statements a DB connector needs before ATTACH.
fn install_load(connector: SqlConnector) -> &'static [&'static str] {
    match connector {
        SqlConnector::Postgres => &["INSTALL postgres;", "LOAD postgres;"],
        SqlConnector::Mysql => &["INSTALL mysql;", "LOAD mysql;"],
        SqlConnector::Sqlite => &["INSTALL sqlite;", "LOAD sqlite;"],
        SqlConnector::Erpl => &["LOAD erpl;"],
        SqlConnector::JsonDir | SqlConnector::ParquetDir => &[],
    }
}

/// Form the directory glob for a directory connector. A `relation` that
/// already contains a `*` glob is used verbatim; otherwise the appropriate
/// `*.json` / `*.parquet` glob is appended.
fn directory_glob(connector: SqlConnector, relation: &str) -> String {
    if relation.contains('*') {
        return relation.to_owned();
    }
    let trimmed = relation.trim_end_matches('/');
    let ext = match connector {
        SqlConnector::JsonDir => "json",
        SqlConnector::ParquetDir => "parquet",
        _ => "*",
    };
    format!("{trimmed}/*.{ext}")
}

/// Deterministic, injection-safe view name from `(skill, id)`.
fn view_name(skill: &str, id: &str) -> String {
    format!("vw_{}__{}", sanitize_ident(skill), sanitize_ident(id))
}

fn sanitize_ident(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' {
            out.push(ch.to_ascii_lowercase());
        } else {
            out.push('_');
        }
    }
    if out.is_empty() || out.chars().next().is_some_and(|c| c.is_ascii_digit()) {
        out.insert(0, 'x');
    }
    out
}

/// Whether `s` is a safe unquoted DuckDB identifier (view name / alias).
fn is_valid_identifier(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Reject characters that could break out of a single-quoted SQL literal or
/// stack a second statement (mirrors `is_safe_attach_source`). The relation
/// / filter come from an operator-authored skill page, but we still validate
/// defensively — they are spliced (DuckDB has no binding for these positions).
fn is_safe_sql_fragment(s: &str) -> bool {
    !s.chars()
        .any(|c| c == '\'' || c == '"' || c == ';' || c == '`' || c == '\\' || c.is_control())
}

fn hash_binding(b: &SqlViewBinding) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b.connector.as_str().as_bytes());
    hasher.update([0]);
    hasher.update(b.attach.as_deref().unwrap_or("").as_bytes());
    hasher.update([0]);
    hasher.update(b.relation.as_bytes());
    hasher.update([0]);
    hasher.update(b.filter.as_deref().unwrap_or("").as_bytes());
    hasher.update([0]);
    for (k, v) in &b.project {
        hasher.update(k.as_bytes());
        hasher.update([b'=']);
        hasher.update(v.as_bytes());
        hasher.update([0]);
    }
    for s in &b.search_text {
        hasher.update(s.as_bytes());
        hasher.update([0]);
    }
    hex(&hasher.finalize())
}

/// The view's result schema as `(column_name, column_type)` in order.
/// `DESCRIBE` returns one row per column.
fn describe(conn: &duckdb::Connection, view: &str) -> Result<Vec<(String, String)>, SqlViewError> {
    let mut stmt = conn.prepare(&format!("DESCRIBE {view}"))?;
    let mut rows = stmt.query([])?;
    let mut out = Vec::new();
    while let Some(row) = rows.next()? {
        out.push((row.get(0)?, row.get(1)?));
    }
    Ok(out)
}

/// Capture the view's result schema as a sha256 fingerprint (REQ-NF-06).
fn schema_fingerprint(conn: &duckdb::Connection, view: &str) -> Result<String, SqlViewError> {
    let mut hasher = Sha256::new();
    for (name, ty) in describe(conn, view)? {
        hasher.update(name.as_bytes());
        hasher.update([b':']);
        hasher.update(ty.as_bytes());
        hasher.update([b'\n']);
    }
    Ok(hex(&hasher.finalize()))
}

fn overlay_markdown(
    skill: &str,
    id: &str,
    view: &str,
    binding_hash: &str,
    fingerprint: &str,
    body: &str,
) -> String {
    format!(
        "---\n\
         type: instance\n\
         skill: {skill}\n\
         id: {id}\n\
         backend_ref:\n\
        \x20 kind: sql_view\n\
        \x20 view: {view}\n\
        \x20 binding_hash: {binding_hash}\n\
        \x20 source_schema_fingerprint: {fingerprint}\n\
         ---\n\
         {body}\n"
    )
}

fn duck_value_to_json(row: &duckdb::Row<'_>, i: usize) -> serde_json::Value {
    use serde_json::Value;
    // Try the common types in turn; fall back to a string rendering.
    if let Ok(v) = row.get::<_, Option<i64>>(i) {
        return v.map_or(Value::Null, Value::from);
    }
    if let Ok(v) = row.get::<_, Option<f64>>(i) {
        return v.map_or(Value::Null, Value::from);
    }
    if let Ok(v) = row.get::<_, Option<bool>>(i) {
        return v.map_or(Value::Null, Value::from);
    }
    if let Ok(v) = row.get::<_, Option<String>>(i) {
        return v.map_or(Value::Null, Value::from);
    }
    Value::Null
}

fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attach_sql_is_always_read_only() {
        // The no-write-back invariant (REQ-SQL-04) is encoded in the builder:
        // every DB connector ATTACHes READ_ONLY.
        for c in [
            SqlConnector::Postgres,
            SqlConnector::Mysql,
            SqlConnector::Sqlite,
            SqlConnector::Erpl,
        ] {
            let sql = attach_sql(c, "crm", "dsn-here");
            assert!(
                sql.contains("READ_ONLY"),
                "{c:?} ATTACH must be READ_ONLY: {sql}"
            );
            assert!(sql.contains("crm"));
        }
    }

    #[test]
    fn view_name_is_deterministic_and_safe() {
        let v = view_name("customers.eu", "ACME-001");
        assert_eq!(v, "vw_customers_eu__acme_001");
        assert!(is_valid_identifier(&v));
        // Stable across calls.
        assert_eq!(v, view_name("customers.eu", "ACME-001"));
    }

    #[test]
    fn directory_glob_appends_extension_or_uses_existing_glob() {
        assert_eq!(
            directory_glob(SqlConnector::JsonDir, "/data/customers"),
            "/data/customers/*.json"
        );
        assert_eq!(
            directory_glob(SqlConnector::ParquetDir, "/data/p/"),
            "/data/p/*.parquet"
        );
        assert_eq!(
            directory_glob(SqlConnector::JsonDir, "/data/**/*.json"),
            "/data/**/*.json"
        );
    }

    #[test]
    fn rejects_unsafe_sql_fragments() {
        assert!(!is_safe_sql_fragment("x'; DROP TABLE pages; --"));
        assert!(is_safe_sql_fragment("region = EU"));
    }
}
