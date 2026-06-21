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
        let fingerprint = self.materialise_view(&view, binding).await?;
        let binding_hash = hash_binding(binding);
        let page_id = format!("markdown/instances/{skill}/{instance_id}.md");
        let content = overlay_markdown(
            skill,
            instance_id,
            &view,
            binding,
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

    /// `CREATE OR REPLACE` the named view from a binding (INSTALL/LOAD +
    /// READ_ONLY ATTACH as needed) and return its current schema
    /// fingerprint. Shared by create, rebuild-reconstruct, and
    /// validate-reprobe — so the three paths can never diverge.
    pub async fn materialise_view(
        &self,
        view: &str,
        binding: &SqlViewBinding,
    ) -> Result<String, SqlViewError> {
        materialise_view_on(&self.indexer, view, binding, self.allow_unsigned_extensions).await
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
}

/// `CREATE OR REPLACE` the named view from a binding on `indexer`'s
/// connection and return its current schema fingerprint. Free function so
/// the create path (`SqlViewBackend`), rebuild-reconstruct, and
/// validate-reprobe — which only hold `&Indexer` — all share one code path.
pub(crate) async fn materialise_view_on(
    indexer: &Indexer,
    view: &str,
    binding: &SqlViewBinding,
    allow_unsigned: bool,
) -> Result<String, SqlViewError> {
    if !is_valid_identifier(view) {
        return Err(SqlViewError::InvalidBinding(format!(
            "not a valid view identifier: {view:?}"
        )));
    }
    let source_expr = prepare_source(indexer, binding, allow_unsigned).await?;
    let filter_sql = match binding.filter.as_deref() {
        Some(f) if !f.trim().is_empty() => {
            if !is_safe_sql_fragment(f) || !is_safe_filter(f) {
                return Err(SqlViewError::InvalidBinding(format!(
                    "filter contains an unsafe character or keyword: {f:?}"
                )));
            }
            format!(" WHERE {f}")
        }
        _ => String::new(),
    };
    let conn = indexer.conn.lock().await;
    conn.execute_batch(&format!(
        "CREATE OR REPLACE VIEW {view} AS SELECT * FROM {source_expr}{filter_sql}"
    ))?;
    schema_fingerprint(&conn, view)
}

/// Resolve the FROM-clause source expression, performing any required
/// INSTALL/LOAD + READ_ONLY ATTACH first. Directory connectors need no
/// credential; DB connectors dereference the admin credential registry.
async fn prepare_source(
    indexer: &Indexer,
    binding: &SqlViewBinding,
    allow_unsigned: bool,
) -> Result<String, SqlViewError> {
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
            let cred = indexer
                .lookup_credential(attach)
                .await?
                .ok_or_else(|| SqlViewError::CredentialNotFound(attach.to_owned()))?;
            if matches!(db, SqlConnector::Erpl) && !allow_unsigned {
                return Err(SqlViewError::UnsignedExtensionNotAllowed);
            }
            if !is_safe_sql_fragment(&cred.secret) {
                return Err(SqlViewError::InvalidBinding(
                    "registered secret contains an unsafe character".to_owned(),
                ));
            }
            if !is_valid_db_relation(&binding.relation) || !is_valid_identifier(attach) {
                return Err(SqlViewError::InvalidBinding(
                    "relation or attach name is unsafe".to_owned(),
                ));
            }
            let conn = indexer.conn.lock().await;
            if matches!(db, SqlConnector::Erpl) && allow_unsigned {
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

/// Late-materialised SQL-view search candidates (PR-2d). See
/// [`crate::Indexer::sql_view_search_candidates`]. Returns candidates only —
/// the dispatcher ACL-filters before fusion (INV-ACL-FUSION).
pub(crate) async fn search_candidates(
    indexer: &Indexer,
    q: &str,
    skill_filter: Option<&str>,
) -> Result<Vec<crate::search::SearchHit>, crate::IndexerError> {
    use escurel_md::PageType;

    // 1. Enumerate sql_view overlay pages (release the lock before the
    //    per-instance work, which re-locks for skill_backend + the match
    //    query — the connection mutex is not reentrant).
    struct Row {
        page_id: String,
        slug: Option<String>,
        skill: String,
        frontmatter: serde_json::Value,
        view: String,
    }
    let rows: Vec<Row> = {
        let conn = indexer.conn.lock().await;
        let mut sql = String::from(
            "SELECT page_id, slug, skill, frontmatter::VARCHAR, \
             json_extract_string(frontmatter, '$.backend_ref.view') AS view \
             FROM pages \
             WHERE page_type = 'instance' \
               AND json_extract_string(frontmatter, '$.backend_ref.kind') = 'sql_view'",
        );
        if skill_filter.is_some() {
            sql.push_str(" AND skill = ?");
        }
        let mut stmt = conn.prepare(&sql)?;
        let map_row = |r: &duckdb::Row<'_>| {
            let fm_json: String = r.get(3)?;
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<String>>(1)?,
                r.get::<_, String>(2)?,
                fm_json,
                r.get::<_, Option<String>>(4)?,
            ))
        };
        // (page_id, slug, skill, frontmatter_json, view)
        type RawRow = (String, Option<String>, String, String, Option<String>);
        let raw: Vec<RawRow> = if let Some(s) = skill_filter {
            stmt.query_map(duckdb::params![s], map_row)?
                .collect::<Result<_, _>>()?
        } else {
            stmt.query_map([], map_row)?.collect::<Result<_, _>>()?
        };
        raw.into_iter()
            .filter_map(|(page_id, slug, skill, fm_json, view)| {
                let frontmatter: serde_json::Value = serde_json::from_str(&fm_json).ok()?;
                Some(Row {
                    page_id,
                    slug,
                    skill,
                    frontmatter,
                    view: view?,
                })
            })
            .collect()
    };

    // 2. Per instance: match q against the skill's search_text columns.
    let pattern = format!("%{q}%");
    let mut hits = Vec::new();
    for row in rows {
        let Ok(binding) = indexer.skill_backend(&row.skill).await else {
            continue;
        };
        let Some(sv) = binding.sql_view else { continue };
        let cols: Vec<&String> = sv
            .search_text
            .iter()
            .filter(|c| is_valid_identifier(c))
            .collect();
        if cols.is_empty() || !is_managed_view(&row.view) {
            continue;
        }
        let where_clause = cols
            .iter()
            .map(|c| format!("{c} ILIKE ?"))
            .collect::<Vec<_>>()
            .join(" OR ");
        let sql = format!("SELECT count(*) FROM {} WHERE {where_clause}", row.view);
        let count: i64 = {
            let conn = indexer.conn.lock().await;
            let binds: Vec<&str> = cols.iter().map(|_| pattern.as_str()).collect();
            conn.query_row(&sql, duckdb::params_from_iter(binds), |r| r.get(0))
                .unwrap_or(0)
        };
        if count > 0 {
            hits.push(crate::search::SearchHit {
                page_id: row.page_id,
                slug: row.slug,
                skill: row.skill,
                page_type: PageType::Instance,
                anchor: None,
                snippet: format!("[sql_view {}] matched {count} row(s)", row.view),
                score: count as f64,
                frontmatter_excerpt: row.frontmatter,
            });
        }
    }
    hits.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    Ok(hits)
}

/// Read up to `limit` rows from a materialised view as JSON objects, on an
/// already-locked connection. Shared by [`SqlViewBackend::project_rows`] and
/// [`crate::Indexer::project_view`] (the read-path projection, PR-2c).
pub(crate) fn project_view_rows(
    conn: &duckdb::Connection,
    view: &str,
    limit: usize,
) -> Result<Vec<serde_json::Map<String, serde_json::Value>>, SqlViewError> {
    // Defence in depth (with the backend_ref-immutability guard): the read
    // path only ever projects escurel-managed views (the deterministic `vw_`
    // names from `view_name`), so a tampered binding can never make `expand`
    // `SELECT *` a server-side table like `external_credentials`.
    if !is_managed_view(view) {
        return Err(SqlViewError::InvalidBinding(format!(
            "not a managed sql-view identifier (expected `vw_…`): {view:?}"
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

/// Whether `view` is an escurel-managed SQL view: a safe identifier with the
/// deterministic `vw_` prefix that `view_name` produces. The read path only
/// projects these, so a tampered `backend_ref.view` can't point the read path
/// at an arbitrary server-side table.
fn is_managed_view(view: &str) -> bool {
    is_valid_identifier(view) && view.starts_with("vw_")
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

/// Validate that a relation name is strictly a dot-separated sequence of
/// valid unquoted identifiers.
fn is_valid_db_relation(s: &str) -> bool {
    !s.is_empty() && s.split('.').all(is_valid_identifier)
}

/// Reject comments and SQL command keywords in filters.
fn is_safe_filter(s: &str) -> bool {
    let s_lower = s.to_lowercase();
    if s_lower.contains("--") || s_lower.contains("/*") || s_lower.contains("*/") {
        return false;
    }
    let forbidden_keywords = [
        "select", "union", "insert", "update", "delete", "drop", "alter", "create", "replace",
        "from",
    ];
    for kw in forbidden_keywords {
        if let Some(mut idx) = s_lower.find(kw) {
            while idx != usize::MAX {
                let before = if idx == 0 {
                    ' '
                } else {
                    s_lower.as_bytes()[idx - 1] as char
                };
                let after = if idx + kw.len() == s_lower.len() {
                    ' '
                } else {
                    s_lower.as_bytes()[idx + kw.len()] as char
                };
                if !before.is_alphanumeric()
                    && before != '_'
                    && !after.is_alphanumeric()
                    && after != '_'
                {
                    return false;
                }
                if let Some(next) = s_lower[idx + 1..].find(kw) {
                    idx = idx + 1 + next;
                } else {
                    idx = usize::MAX;
                }
            }
        }
    }
    true
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
pub(crate) fn schema_fingerprint(
    conn: &duckdb::Connection,
    view: &str,
) -> Result<String, SqlViewError> {
    let mut hasher = Sha256::new();
    for (name, ty) in describe(conn, view)? {
        hasher.update(name.as_bytes());
        hasher.update([b':']);
        hasher.update(ty.as_bytes());
        hasher.update([b'\n']);
    }
    Ok(hex(&hasher.finalize()))
}

#[allow(clippy::too_many_arguments)]
fn overlay_markdown(
    skill: &str,
    id: &str,
    view: &str,
    binding: &SqlViewBinding,
    binding_hash: &str,
    fingerprint: &str,
    body: &str,
) -> String {
    // The `source` sub-block carries everything `rebuild` needs to
    // reconstruct the view (REQ-NF-01) — connector + relation + the
    // credential *name* (never the secret, REQ-SQL-05) + optional filter.
    let mut source = format!(
        "    connector: {}\n    relation: {}\n",
        binding.connector.as_str(),
        binding.relation
    );
    if let Some(attach) = &binding.attach {
        source.push_str(&format!("    attach: {attach}\n"));
    }
    if let Some(filter) = &binding.filter {
        source.push_str(&format!("    filter: {filter:?}\n"));
    }
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
        \x20 source:\n\
         {source}\
         ---\n\
         {body}\n"
    )
}

/// Health of one SQL-view binding, from a re-probe (REQ-NF-06).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BindingStatus {
    pub page_id: String,
    pub view: String,
    /// `ok` | `binding_degraded` | `backend_unavailable`.
    pub status: String,
    pub detail: Option<String>,
}

/// Parse the `backend_ref.source` sub-block back into a [`SqlViewBinding`]
/// for reconstruction / re-probe.
pub(crate) fn parse_source_binding(backend_ref: &serde_json::Value) -> Option<SqlViewBinding> {
    let source = backend_ref.get("source")?.as_object()?;
    let connector = SqlConnector::from_wire(
        source
            .get("connector")
            .and_then(serde_json::Value::as_str)?,
    )?;
    let relation = source
        .get("relation")
        .and_then(serde_json::Value::as_str)?
        .to_owned();
    let attach = source
        .get("attach")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned);
    let filter = source
        .get("filter")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned);
    Some(SqlViewBinding {
        connector,
        attach,
        relation,
        filter,
        project: std::collections::BTreeMap::new(),
        search_text: Vec::new(),
    })
}

/// Every `sql_view` overlay's `(page_id, skill, view, backend_ref)`.
async fn enumerate_sql_view_overlays(
    indexer: &Indexer,
) -> Result<Vec<(String, String, String, serde_json::Value)>, crate::IndexerError> {
    let conn = indexer.conn.lock().await;
    let mut stmt = conn.prepare(
        "SELECT page_id, skill, frontmatter::VARCHAR FROM pages \
         WHERE page_type = 'instance' \
           AND json_extract_string(frontmatter, '$.backend_ref.kind') = 'sql_view'",
    )?;
    let rows: Vec<(String, String, String)> = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?
        .collect::<Result<_, _>>()?;
    Ok(rows
        .into_iter()
        .filter_map(|(page_id, skill, fm_json)| {
            let fm: serde_json::Value = serde_json::from_str(&fm_json).ok()?;
            let view = fm.get("backend_ref")?.get("view")?.as_str()?.to_owned();
            let backend_ref = fm.get("backend_ref")?.clone();
            Some((page_id, skill, view, backend_ref))
        })
        .collect())
}

/// Reconstruct every SQL view from its overlay's `backend_ref.source`
/// (rebuild step, REQ-NF-01). No data to rebuild — external — just the view.
pub(crate) async fn reconstruct_views(indexer: &Indexer) -> Result<(), crate::IndexerError> {
    let overlays = enumerate_sql_view_overlays(indexer).await?;
    for (_page_id, _skill, view, backend_ref) in overlays {
        if let Some(binding) = parse_source_binding(&backend_ref) {
            // Best-effort: a source that is offline now is reported by
            // validate_bindings, not fatal to the whole rebuild.
            let _ = materialise_view_on(indexer, &view, &binding, false).await;
        }
    }
    Ok(())
}

/// Re-probe every SQL-view binding and compare the current schema
/// fingerprint to the one stored at create time (REQ-NF-06).
pub(crate) async fn validate_all_bindings(
    indexer: &Indexer,
) -> Result<Vec<BindingStatus>, crate::IndexerError> {
    let overlays = enumerate_sql_view_overlays(indexer).await?;
    let mut out = Vec::new();
    for (page_id, _skill, view, backend_ref) in overlays {
        let stored = backend_ref
            .get("source_schema_fingerprint")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let (status, detail) = match parse_source_binding(&backend_ref) {
            None => (
                "backend_unavailable".to_owned(),
                Some("backend_ref.source missing or unparseable".to_owned()),
            ),
            Some(binding) => match materialise_view_on(indexer, &view, &binding, false).await {
                Err(e) => ("backend_unavailable".to_owned(), Some(e.to_string())),
                Ok(current) if current == stored => ("ok".to_owned(), None),
                Ok(current) => (
                    "binding_degraded".to_owned(),
                    Some(format!("schema fingerprint drift: {stored} → {current}")),
                ),
            },
        };
        out.push(BindingStatus {
            page_id,
            view,
            status,
            detail,
        });
    }
    Ok(out)
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
    fn only_vw_prefixed_views_are_projectable() {
        // Defence in depth: the read path must refuse server-side tables.
        assert!(is_managed_view("vw_customers__eu"));
        assert!(!is_managed_view("external_credentials"));
        assert!(!is_managed_view("pages"));
        assert!(!is_managed_view("group_members"));
        assert!(!is_managed_view("blocks"));
    }

    #[test]
    fn rejects_unsafe_sql_fragments() {
        assert!(!is_safe_sql_fragment("x'; DROP TABLE pages; --"));
        assert!(is_safe_sql_fragment("region = EU"));
    }

    #[test]
    fn validates_db_relations() {
        assert!(is_valid_db_relation("public.customers"));
        assert!(is_valid_db_relation("customers"));
        assert!(!is_valid_db_relation(
            "my_table UNION SELECT * FROM external_credentials"
        ));
    }

    #[test]
    fn validates_filters() {
        assert!(is_safe_filter("region = EU"));
        assert!(!is_safe_filter(
            "id = 0 UNION SELECT * FROM external_credentials"
        ));
        assert!(!is_safe_filter("id = 1 -- comment"));
    }
}
