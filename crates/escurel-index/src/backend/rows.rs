//! Per-row instances over a `sql_view` skill (`backend.instances: rows`).
//!
//! The rows are VIRTUAL: nothing is stored per row. A row's identity is its key column(s), encoded
//! into the instance id by a PURE, reversible function (so there is no id table to keep in sync); the
//! list is a KEYSET page over the skill's one managed view; a read is one bound-parameter lookup.
//! The optional linked markdown is the ordinary stored page at the same page id — the server merges
//! the two on read.
//!
//! Safety: column names only ever come from `DESCRIBE` of the view (never from the caller), values
//! are always BOUND, and the only spliced SQL is the admin-authored `source.filter`, which
//! `materialise_view_on` already validates.

use std::collections::BTreeMap;

use duckdb::types::ValueRef;
use serde_json::{Map, Value};

use super::binding::{RowsConfig, SqlViewBinding};
use super::sql_view::{
    SqlViewError, describe, is_valid_identifier, materialise_view_on, sanitize_ident,
};
use crate::Indexer;

/// Hard cap on one list page (the same ceiling `list_instances` has for stored pages).
pub const ROWS_MAX_LIMIT: usize = 10_000;

/// A page also stops at this many bytes of row data (at least one row is always returned), so a
/// wide table cannot make one request hold every column of 10,000 rows in memory.
pub const ROWS_MAX_PAGE_BYTES: usize = 8 * 1024 * 1024;

/// How long one source query may run before it is interrupted.
pub const ROWS_QUERY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// Runs `f` with a watchdog that interrupts the connection's running statement after `timeout`. The
/// watchdog is joined before this returns, so a late interrupt can never hit the NEXT statement.
pub(crate) fn with_statement_timeout<T, E: From<SqlViewError>>(
    conn: &duckdb::Connection,
    timeout: std::time::Duration,
    f: impl FnOnce() -> Result<T, E>,
) -> Result<T, E> {
    let handle = conn.interrupt_handle();
    let (done, wait) = std::sync::mpsc::channel::<()>();
    let fired = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let fired_in = std::sync::Arc::clone(&fired);
    let watchdog = std::thread::spawn(move || {
        if wait
            .recv_timeout(timeout)
            .is_err_and(|e| e == std::sync::mpsc::RecvTimeoutError::Timeout)
        {
            fired_in.store(true, std::sync::atomic::Ordering::SeqCst);
            handle.interrupt();
        }
    });
    let started = std::time::Instant::now();
    let result = f();
    let overran = started.elapsed() >= timeout;
    let _ = done.send(());
    let _ = watchdog.join();
    match result {
        // `overran`: a Postgres server cancels at the same instant as the watchdog fires (see
        // `with_server_statement_timeout`), so an error that arrives past the deadline is the timeout.
        Err(_) if overran || fired.load(std::sync::atomic::Ordering::SeqCst) => {
            Err(E::from(SqlViewError::InvalidBinding(format!(
                "backend_unavailable: the source query did not answer within {}s and was \
                 interrupted; narrow the list with a filter or ask the source's owner",
                timeout.as_secs().max(1)
            ))))
        }
        other => other,
    }
}

/// A `rows` skill's resolved binding.
#[derive(Debug, Clone)]
pub struct RowsSource {
    pub skill: String,
    pub sql: SqlViewBinding,
    pub cfg: RowsConfig,
    /// Source column → instance frontmatter field (the skill's `project`).
    pub project: BTreeMap<String, String>,
    /// The ONE managed view for the whole skill.
    pub view: String,
}

/// One row, as an instance.
#[derive(Debug, Clone)]
pub struct RowRecord {
    /// The instance id (the encoded key).
    pub id: String,
    /// `markdown/instances/<skill>/<id>.md` — the same id a linked page would have.
    pub page_id: String,
    /// Every source column, raw.
    pub columns: Map<String, Value>,
    /// The projected columns under their frontmatter names (all columns when the skill declares no
    /// `project`), typed.
    pub fields: Map<String, Value>,
    /// `(column, DuckDB type)` of the source relation, from `DESCRIBE` — the discovered schema.
    pub types: Vec<(String, String)>,
    /// Every source column as DuckDB's own `CAST(col AS VARCHAR)` text (`None` = NULL), captured by
    /// [`Indexer::rows_get`] only: the basis a write-back's optimistic check compares against, in the
    /// same representation the `UPDATE` compares with, so no value is ever round-tripped through JSON.
    pub source_texts: Vec<(String, Option<String>)>,
}

/// The escurel field kind a DuckDB column type maps to (the skill's own `fields:` override it).
#[must_use]
pub fn field_kind_for(duck_type: &str) -> &'static str {
    let t = duck_type.to_ascii_uppercase();
    match t.as_str() {
        "BOOLEAN" => "bool",
        "TINYINT" | "SMALLINT" | "INTEGER" | "BIGINT" | "HUGEINT" | "UTINYINT" | "USMALLINT"
        | "UINTEGER" | "UBIGINT" | "UHUGEINT" => "int",
        "DOUBLE" | "FLOAT" | "REAL" => "float",
        "DATE" => "date",
        _ if t.starts_with("DECIMAL") => "float",
        _ if t.starts_with("TIMESTAMP") => "datetime",
        _ => "string",
    }
}

#[derive(Debug, Clone)]
pub struct RowsPage {
    pub rows: Vec<RowRecord>,
    /// `Some` ⇒ more rows; `None` ⇒ the end.
    pub next_cursor: Option<String>,
}

/// The managed view for a skill's rows (`vw_<skill>__rows`; the `vw_` prefix is what
/// `is_managed_view` allow-lists).
#[must_use]
pub fn rows_view_name(skill: &str) -> String {
    format!("vw_{}__rows", sanitize_ident(skill))
}

/// Encode key values into an instance id: parts pass through when they are `[A-Za-z0-9._-]`, every
/// other byte becomes `~XX`; for a composite key `-` is escaped too so the join is unambiguous.
#[must_use]
pub fn encode_row_id(parts: &[String]) -> String {
    let composite = parts.len() > 1;
    parts
        .iter()
        .map(|p| encode_part(p, composite))
        .collect::<Vec<_>>()
        .join("-")
}

/// The inverse of [`encode_row_id`] for a skill whose key has `arity` columns. `None` when the id
/// is not a well-formed encoding (a stray `~`, a wrong number of parts).
#[must_use]
pub fn decode_row_id(id: &str, arity: usize) -> Option<Vec<String>> {
    if arity == 0 {
        return None;
    }
    let raw: Vec<&str> = if arity == 1 {
        vec![id]
    } else {
        id.split('-').collect()
    };
    if raw.len() != arity {
        return None;
    }
    raw.into_iter().map(decode_part).collect()
}

fn encode_part(p: &str, composite: bool) -> String {
    let mut out = String::with_capacity(p.len());
    for b in p.bytes() {
        let plain =
            b.is_ascii_alphanumeric() || b == b'.' || b == b'_' || (b == b'-' && !composite);
        if plain {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("~{b:02X}"));
        }
    }
    out
}

fn decode_part(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'~' {
            let hex = s.get(i + 1..i + 3)?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// The page id of instance `id` of `skill`: `markdown/instances/<skill>/<id>.md`. The one place the
/// format is built; `split_instance_page_id` is its inverse.
#[must_use]
pub fn instance_page_id(skill: &str, id: &str) -> String {
    format!("markdown/instances/{skill}/{id}.md")
}

/// `markdown/instances/<skill>/<id>.md`, parsed back; `None` for any other page id.
#[must_use]
pub fn split_instance_page_id(page_id: &str) -> Option<(&str, &str)> {
    let rest = page_id.strip_prefix("markdown/instances/")?;
    let rest = rest.strip_suffix(".md")?;
    let (skill, id) = rest.split_once('/')?;
    (!skill.is_empty() && !id.is_empty() && !id.contains('/')).then_some((skill, id))
}

impl Indexer {
    /// The skill's rows binding, or `None` when it is not a `rows` skill. A `rows` skill whose
    /// binding cannot be used (no key, no usable source) is an error naming what is missing — never
    /// a silent fall-back to whole-view behaviour.
    pub async fn rows_source(&self, skill: &str) -> Result<Option<RowsSource>, SqlViewError> {
        let b = self.skill_backend(skill).await?;
        let Some(cfg) = b.rows else {
            return Ok(None);
        };
        // A REST/MCP rows skill (stage 4) is served by the gateway's connector, not by DuckDB.
        if b.remote.is_some() {
            return Ok(None);
        }
        let sql = b.sql_view.ok_or_else(|| {
            SqlViewError::InvalidBinding(format!(
                "skill `{skill}` declares `instances: rows` but has no usable `source:`"
            ))
        })?;
        if cfg.key.is_empty() {
            return Err(SqlViewError::InvalidBinding(format!(
                "skill `{skill}` declares `instances: rows` but no `key:` column"
            )));
        }
        let project = sql.project.clone();
        Ok(Some(RowsSource {
            skill: skill.to_owned(),
            sql,
            cfg,
            project,
            view: rows_view_name(skill),
        }))
    }

    /// One keyset page of rows, ascending by key. `filter` is `(column, value)` and only a
    /// `filterable:` column is accepted; the value is bound, never spliced. `cursor` is the opaque
    /// token of a previous page.
    pub async fn rows_list(
        &self,
        src: &RowsSource,
        cursor: Option<&str>,
        limit: usize,
        filter: Option<(&str, &str)>,
    ) -> Result<RowsPage, SqlViewError> {
        let limit = limit.clamp(1, ROWS_MAX_LIMIT);
        materialise_view_on(self, &src.view, &src.sql, false).await?;
        let conn = self.conn.lock().await;
        let cols = describe(&conn, &src.view)?;
        let names: Vec<&str> = cols.iter().map(|(n, _)| n.as_str()).collect();
        let key_exprs = key_exprs(src, &names)?;

        let mut wheres: Vec<String> = Vec::new();
        let mut params: Vec<String> = Vec::new();
        if let Some((field, value)) = filter {
            // The caller names a FRONTMATTER field; map it back to the source column.
            let not_filterable = || {
                SqlViewError::InvalidBinding(format!(
                    "`{field}` is not a filterable field of `{}`; filterable: {}; \
                     use `search` for free text",
                    src.skill,
                    filterable_names(src)
                ))
            };
            let col = column_for_field(src, field).ok_or_else(not_filterable)?;
            if !src.cfg.filterable.iter().any(|f| f == &col) || !names.contains(&col.as_str()) {
                return Err(not_filterable());
            }
            wheres.push(format!("CAST(\"{col}\" AS VARCHAR) = ?"));
            params.push(value.to_owned());
        }
        // A row with a NULL key has no identity (its id would be empty): it is not an instance, and
        // keeping it out of the listing is what stops a cursor from ever being built from one.
        for k in &src.cfg.key {
            wheres.push(format!("\"{k}\" IS NOT NULL"));
        }
        if let Some(token) = cursor {
            let after = decode_cursor(token, key_exprs.len())?;
            let tuple = key_exprs.join(", ");
            let marks = vec!["?"; key_exprs.len()].join(", ");
            wheres.push(format!("({tuple}) > ({marks})"));
            params.extend(after);
        }
        let where_sql = if wheres.is_empty() {
            String::new()
        } else {
            format!(" WHERE {}", wheres.join(" AND "))
        };
        let sql = format!(
            "SELECT {} FROM {}{where_sql} ORDER BY {} LIMIT {}",
            select_with_keys(&key_exprs),
            src.view,
            key_exprs.join(", "),
            limit + 1
        );
        let timeout = self.rows_query_timeout;
        let (out, cast_keys_seen, more) =
            with_statement_timeout::<_, SqlViewError>(&conn, timeout, || {
                let mut stmt = conn.prepare(&sql)?;
                let mut rows = stmt.query(duckdb::params_from_iter(params.iter()))?;
                let mut out: Vec<RowRecord> = Vec::new();
                // The key of each row as `CAST(.. AS VARCHAR)` renders it: the SAME text the ORDER BY and
                // the `>` comparison use (see `select_with_keys`).
                let mut cast_keys_seen: Vec<Vec<String>> = Vec::new();
                let mut bytes = 0usize;
                let mut more = false;
                while let Some(row) = rows.next()? {
                    if out.len() == limit {
                        more = true;
                        break;
                    }
                    let keys = cast_keys(row, cols.len(), src.cfg.key.len())?;
                    let rec = read_record(src, row, &cols, &keys)?;
                    bytes += serde_json::to_string(&rec.columns).map_or(0, |s| s.len());
                    out.push(rec);
                    cast_keys_seen.push(keys);
                    // Stop on bytes (not just rows), but never return an empty page.
                    if bytes >= ROWS_MAX_PAGE_BYTES {
                        more = rows.next()?.is_some();
                        break;
                    }
                }
                Ok((out, cast_keys_seen, more))
            })?;
        let next_cursor = if more {
            cast_keys_seen.last().map(|k| encode_cursor(k))
        } else {
            None
        };
        Ok(RowsPage {
            rows: out,
            next_cursor,
        })
    }

    /// The one row with instance id `id`, or `None` when the source has no such row.
    pub async fn rows_get(
        &self,
        src: &RowsSource,
        id: &str,
    ) -> Result<Option<RowRecord>, SqlViewError> {
        let Some(values) = decode_row_id(id, src.cfg.key.len()) else {
            return Ok(None);
        };
        materialise_view_on(self, &src.view, &src.sql, false).await?;
        let conn = self.conn.lock().await;
        let cols = describe(&conn, &src.view)?;
        let names: Vec<&str> = cols.iter().map(|(n, _)| n.as_str()).collect();
        let exprs = key_exprs(src, &names)?;
        let wheres: Vec<String> = exprs.iter().map(|e| format!("{e} = ?")).collect();
        // Every column's VARCHAR text rides along after the key texts: the write-back basis.
        let texts: Vec<String> = cols
            .iter()
            .enumerate()
            .map(|(i, (n, _))| format!("CAST(\"{n}\" AS VARCHAR) AS \"__escurel_v{i}\""))
            .collect();
        let sql = format!(
            "SELECT {}, {} FROM {} WHERE {} LIMIT 1",
            select_with_keys(&exprs),
            texts.join(", "),
            src.view,
            wheres.join(" AND ")
        );
        let mut stmt = conn.prepare(&sql)?;
        let mut rows = stmt.query(duckdb::params_from_iter(values.iter()))?;
        match rows.next()? {
            Some(row) => {
                let keys = cast_keys(row, cols.len(), src.cfg.key.len())?;
                let mut rec = read_record(src, row, &cols, &keys)?;
                let base = cols.len() + src.cfg.key.len();
                rec.source_texts = cols
                    .iter()
                    .enumerate()
                    .map(|(i, (n, _))| Ok((n.clone(), row.get::<_, Option<String>>(base + i)?)))
                    .collect::<Result<_, SqlViewError>>()?;
                Ok(Some(rec))
            }
            None => Ok(None),
        }
    }
}

/// How many row hits one `search` takes from ONE rows-backed skill, and in total: a lookup, not a scan.
pub const ROWS_SEARCH_PER_SKILL: usize = 20;
pub const ROWS_SEARCH_TOTAL: usize = 50;

/// `q` as a LIKE pattern that matches it literally anywhere (`%`, `_` and `\` are escaped).
fn like_contains(q: &str) -> String {
    let mut out = String::with_capacity(q.len() + 2);
    out.push('%');
    for c in q.chars() {
        if matches!(c, '%' | '_' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('%');
    out
}

impl Indexer {
    /// Up to `limit` rows whose KEY or one of whose `filterable:` / `searchable:` columns contains `q`
    /// (case-insensitive), in key order. Only those columns are searched: a column the skill did not
    /// declare is never matched, so a search cannot be used to probe data the skill keeps back. `q` is
    /// a bound parameter (a LIKE pattern with its wildcards escaped), never spliced into the SQL.
    pub async fn rows_search(
        &self,
        src: &RowsSource,
        q: &str,
        limit: usize,
    ) -> Result<Vec<RowRecord>, SqlViewError> {
        let q = q.trim();
        if q.is_empty() {
            return Ok(Vec::new());
        }
        let limit = limit.clamp(1, ROWS_MAX_LIMIT);
        materialise_view_on(self, &src.view, &src.sql, false).await?;
        let conn = self.conn.lock().await;
        let cols = describe(&conn, &src.view)?;
        let names: Vec<&str> = cols.iter().map(|(n, _)| n.as_str()).collect();
        let key_exprs = key_exprs(src, &names)?;
        let mut searchable: Vec<&str> = src.cfg.key.iter().map(String::as_str).collect();
        for c in src.cfg.filterable.iter().chain(&src.cfg.searchable) {
            if names.contains(&c.as_str()) && !searchable.contains(&c.as_str()) {
                searchable.push(c.as_str());
            }
        }
        let ors: Vec<String> = searchable
            .iter()
            .map(|c| format!("CAST(\"{c}\" AS VARCHAR) ILIKE ? ESCAPE '\\'"))
            .collect();
        let params: Vec<String> = searchable.iter().map(|_| like_contains(q)).collect();
        let mut wheres = vec![format!("({})", ors.join(" OR "))];
        for k in &src.cfg.key {
            wheres.push(format!("\"{k}\" IS NOT NULL"));
        }
        let sql = format!(
            "SELECT {} FROM {} WHERE {} ORDER BY {} LIMIT {limit}",
            select_with_keys(&key_exprs),
            src.view,
            wheres.join(" AND "),
            key_exprs.join(", ")
        );
        let timeout = self.rows_query_timeout;
        with_statement_timeout(&conn, timeout, || {
            let mut stmt = conn.prepare(&sql)?;
            let mut rows = stmt.query(duckdb::params_from_iter(params.iter()))?;
            let mut out = Vec::new();
            while let Some(row) = rows.next()? {
                let keys = cast_keys(row, cols.len(), src.cfg.key.len())?;
                out.push(read_record(src, row, &cols, &keys)?);
            }
            Ok(out)
        })
    }

    /// Search CANDIDATES from every rows-backed (DuckDB) skill: page-grain hits for the rows `q`
    /// matches, at most [`ROWS_SEARCH_PER_SKILL`] per skill and [`ROWS_SEARCH_TOTAL`] overall, ranked by
    /// key order. Like the SQL-view lane these are candidates only: the dispatcher applies the
    /// fail-closed row ACL BEFORE fusion. A skill whose source cannot be read is skipped, not fatal.
    pub async fn rows_search_candidates(
        &self,
        q: &str,
        skill_filter: Option<&str>,
    ) -> Result<Vec<crate::search::SearchHit>, crate::IndexerError> {
        use crate::search::SearchHit;
        let mut hits: Vec<SearchHit> = Vec::new();
        for skill in self.list_skills().await? {
            if skill_filter.is_some_and(|f| f != skill.id) || hits.len() >= ROWS_SEARCH_TOTAL {
                continue;
            }
            let Ok(Some(src)) = self.rows_source(&skill.id).await else {
                continue;
            };
            let Ok(rows) = self.rows_search(&src, q, ROWS_SEARCH_PER_SKILL).await else {
                continue;
            };
            for rec in rows {
                let rank = hits.len();
                hits.push(SearchHit {
                    snippet: matched_snippet(&src, &rec, q),
                    page_id: rec.page_id,
                    slug: Some(rec.id),
                    skill: skill.id.clone(),
                    page_kind: escurel_md::PageKind::Instance,
                    anchor: None,
                    score: 1.0 / (1.0 + rank as f64),
                    similarity: 0.0,
                    frontmatter_excerpt: Value::Object(rec.fields),
                });
                if hits.len() >= ROWS_SEARCH_TOTAL {
                    break;
                }
            }
        }
        Ok(hits)
    }
}

/// What matched, for a hit's snippet: the first searched column (key first) whose value contains `q`,
/// shown under its frontmatter name (`sold_to = 1000007`).
fn matched_snippet(src: &RowsSource, rec: &RowRecord, q: &str) -> String {
    let needle = q.trim().to_lowercase();
    let searched = src
        .cfg
        .key
        .iter()
        .chain(src.cfg.filterable.iter())
        .chain(src.cfg.searchable.iter());
    for col in searched {
        let Some(v) = rec.columns.get(col) else {
            continue;
        };
        let text = match v {
            Value::String(s) => s.clone(),
            other => other.to_string(),
        };
        if text.to_lowercase().contains(&needle) {
            let name = src.project.get(col).map_or(col.as_str(), String::as_str);
            return format!("{name} = {text}");
        }
    }
    String::new()
}

/// `SELECT` list: every column, then each key as DuckDB's own `CAST(.. AS VARCHAR)` text under the
/// alias `__escurel_key<j>`. The id of a row and the cursor after it are built from THESE texts, so
/// they are exactly what the ORDER BY, the `>` comparison and a lookup (`CAST(col AS VARCHAR) = ?`)
/// see, whatever the key's type (a TIMESTAMP or DECIMAL has no other faithful text form).
fn select_with_keys(key_exprs: &[String]) -> String {
    let keys: Vec<String> = key_exprs
        .iter()
        .enumerate()
        .map(|(j, e)| format!("{e} AS \"__escurel_key{j}\""))
        .collect();
    format!("*, {}", keys.join(", "))
}

/// The cast key texts [`select_with_keys`] appended after the `ncols` view columns. A NULL key has
/// no identity and is filtered out by the caller; it reads as an error here, never as an empty id.
fn cast_keys(
    row: &duckdb::Row<'_>,
    ncols: usize,
    nkeys: usize,
) -> Result<Vec<String>, SqlViewError> {
    (0..nkeys)
        .map(|j| {
            row.get::<_, Option<String>>(ncols + j)?.ok_or_else(|| {
                SqlViewError::InvalidBinding(
                    "a row has a NULL key and cannot be an instance".to_owned(),
                )
            })
        })
        .collect()
}

/// `CAST("<key>" AS VARCHAR)` per key column, validated against the view's real columns.
fn key_exprs(src: &RowsSource, names: &[&str]) -> Result<Vec<String>, SqlViewError> {
    src.cfg
        .key
        .iter()
        .map(|k| {
            if is_valid_identifier(k) && names.contains(&k.as_str()) {
                Ok(format!("CAST(\"{k}\" AS VARCHAR)"))
            } else {
                Err(SqlViewError::InvalidBinding(format!(
                    "key column `{k}` is not a column of the source relation"
                )))
            }
        })
        .collect()
}

/// The source column behind a frontmatter field name (or the column itself when the caller named a
/// column and it is not renamed by `project`).
fn column_for_field(src: &RowsSource, field: &str) -> Option<String> {
    src.project
        .iter()
        .find(|(_, f)| f.as_str() == field)
        .map(|(c, _)| c.clone())
        // The skill DECLARES `filterable` by column name (`kunnr`), while the page shows the projected
        // field (`sold_to`): an agent reading either must be able to filter by the one it saw.
        .or_else(|| is_valid_identifier(field).then(|| field.to_owned()))
}

/// The filterable columns as an agent can name them: `sold_to (column kunnr)`.
fn filterable_names(src: &RowsSource) -> String {
    let names: Vec<String> = src
        .cfg
        .filterable
        .iter()
        .map(|c| match src.project.get(c) {
            Some(f) if f != c => format!("{f} (column {c})"),
            _ => c.clone(),
        })
        .collect();
    if names.is_empty() {
        "none declared".to_owned()
    } else {
        names.join(", ")
    }
}

fn read_record(
    src: &RowsSource,
    row: &duckdb::Row<'_>,
    cols: &[(String, String)],
    keys: &[String],
) -> Result<RowRecord, SqlViewError> {
    let mut columns = Map::new();
    for (i, (name, _ty)) in cols.iter().enumerate() {
        columns.insert(name.clone(), value_to_json(row.get_ref(i)?));
    }
    let id = encode_row_id(keys);
    let mut fields = Map::new();
    if src.project.is_empty() {
        for (c, v) in &columns {
            fields.insert(c.to_ascii_lowercase(), v.clone());
        }
    } else {
        for (col, field) in &src.project {
            if let Some(v) = columns.get(col) {
                fields.insert(field.clone(), v.clone());
            }
        }
    }
    Ok(RowRecord {
        page_id: instance_page_id(&src.skill, &id),
        id,
        columns,
        fields,
        types: cols.to_vec(),
        source_texts: Vec::new(),
    })
}

/// A DuckDB value as JSON, keeping dates and timestamps as ISO strings (the generic fall-through of
/// the older whole-view reader renders them as null).
fn value_to_json(v: ValueRef<'_>) -> Value {
    match v {
        ValueRef::Null => Value::Null,
        ValueRef::Boolean(b) => Value::Bool(b),
        ValueRef::TinyInt(n) => Value::from(n),
        ValueRef::SmallInt(n) => Value::from(n),
        ValueRef::Int(n) => Value::from(n),
        ValueRef::BigInt(n) => Value::from(n),
        ValueRef::UTinyInt(n) => Value::from(n),
        ValueRef::USmallInt(n) => Value::from(n),
        ValueRef::UInt(n) => Value::from(n),
        ValueRef::UBigInt(n) => Value::from(n),
        ValueRef::Float(f) => {
            serde_json::Number::from_f64(f64::from(f)).map_or(Value::Null, Value::Number)
        }
        ValueRef::Double(f) => serde_json::Number::from_f64(f).map_or(Value::Null, Value::Number),
        ValueRef::Text(t) => Value::String(String::from_utf8_lossy(t).into_owned()),
        other => {
            // Dates, timestamps, decimals, hugeints, lists… rendered through DuckDB's own text form.
            let owned = other.to_owned();
            match owned {
                duckdb::types::Value::Date32(d) => Value::String(days_to_iso(i64::from(d))),
                duckdb::types::Value::Decimal(d) => d
                    .to_string()
                    .parse::<f64>()
                    .ok()
                    .and_then(serde_json::Number::from_f64)
                    .map_or(Value::Null, Value::Number),
                duckdb::types::Value::Timestamp(unit, t) => {
                    Value::String(timestamp_to_iso(unit, t))
                }
                v => Value::String(format!("{v:?}")),
            }
        }
    }
}

/// Days since 1970-01-01 → `YYYY-MM-DD` (civil-from-days, proleptic Gregorian).
fn days_to_iso(days: i64) -> String {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}")
}

fn timestamp_to_iso(unit: duckdb::types::TimeUnit, t: i64) -> String {
    use duckdb::types::TimeUnit;
    let micros = match unit {
        TimeUnit::Second => t.saturating_mul(1_000_000),
        TimeUnit::Millisecond => t.saturating_mul(1_000),
        TimeUnit::Microsecond => t,
        TimeUnit::Nanosecond => t / 1_000,
    };
    let secs = micros.div_euclid(1_000_000);
    let day = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    format!(
        "{}T{:02}:{:02}:{:02}Z",
        days_to_iso(day),
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

/// An opaque, versioned cursor: `r1.` + base64url of the last row's key values (a JSON array). It
/// used to be the plain hex of the key, which anyone could read and forge.
const CURSOR_PREFIX: &str = "r1.";

/// Seal `raw` into an opaque, versioned cursor token: `<prefix>` + base64url. Shared by the SQL rows
/// (`r1.`) and the remote rows (`u1.`) so the envelope exists once.
#[must_use]
pub fn seal_cursor(prefix: &str, raw: &[u8]) -> String {
    use base64::Engine as _;
    format!(
        "{prefix}{}",
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(raw)
    )
}

/// The bytes inside a token made by [`seal_cursor`] with the same `prefix`; `None` for anything else
/// (wrong prefix, not base64url).
#[must_use]
pub fn open_cursor(prefix: &str, token: &str) -> Option<Vec<u8>> {
    use base64::Engine as _;
    let body = token.strip_prefix(prefix)?;
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(body.as_bytes())
        .ok()
}

fn encode_cursor(values: &[String]) -> String {
    seal_cursor(
        CURSOR_PREFIX,
        &serde_json::to_vec(values).unwrap_or_default(),
    )
}

fn decode_cursor(token: &str, arity: usize) -> Result<Vec<String>, SqlViewError> {
    let bad = || SqlViewError::InvalidBinding("invalid cursor".to_owned());
    let raw = open_cursor(CURSOR_PREFIX, token).ok_or_else(bad)?;
    let values: Vec<String> = serde_json::from_slice(&raw).map_err(|_| bad())?;
    if values.len() == arity {
        Ok(values)
    } else {
        Err(bad())
    }
}

/// Why a write to a row page was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RowsWriteRejection {
    /// The `Issue` code: `backend_read_only` (the row has nowhere to write), `backend_read_only_field`
    /// (the write touches a source column) or `row_not_found`.
    pub code: &'static str,
    pub location: String,
    pub message: String,
}

impl Indexer {
    /// The write guard for a page id inside a `rows` skill: `None` when the write may go ahead.
    ///
    /// A row page is the row's LINKED MARKDOWN, so a write is allowed only when the skill declares
    /// `linked`, the row exists (or the companion already does — notes outlive their row), the content
    /// carries no server-managed `backend_ref`, and its frontmatter does not carry a projected SOURCE
    /// column: those fields belong to the source and a write could only ever lose to it, so it is
    /// refused loudly instead of being silently dropped on read.
    pub async fn rows_write_rejection(
        &self,
        page_id: &str,
        content: &str,
    ) -> Result<Option<RowsWriteRejection>, SqlViewError> {
        let Some((skill, id)) = split_instance_page_id(page_id) else {
            return Ok(None);
        };
        let Some(src) = self.rows_source(skill).await? else {
            return Ok(None);
        };
        if !src.cfg.linked {
            return Ok(Some(RowsWriteRejection {
                code: "backend_read_only",
                location: "page_id".to_owned(),
                message: format!(
                    "skill `{skill}` is a read-only `rows` backend without `linked` markdown; \
                     its rows cannot be written"
                ),
            }));
        }
        let exists = self.rows_get(&src, id).await?.is_some()
            || self.read_page_markdown(page_id).await?.is_some();
        if !exists {
            return Ok(Some(RowsWriteRejection {
                code: "row_not_found",
                location: "page_id".to_owned(),
                message: format!("`{skill}` has no row with the key `{id}` in its source"),
            }));
        }
        // A malformed draft falls through to the normal validate path.
        let Ok(parsed) = escurel_md::parse(content) else {
            return Ok(None);
        };
        let fields = &parsed.frontmatter.fields;
        if fields.contains_key("backend_ref") {
            return Ok(Some(RowsWriteRejection {
                code: "backend_read_only",
                location: "frontmatter.backend_ref".to_owned(),
                message: "`backend_ref` is server-managed; a row's companion page cannot carry one"
                    .to_owned(),
            }));
        }
        let source_fields: Vec<String> = if src.project.is_empty() {
            Vec::new()
        } else {
            src.project.values().cloned().collect()
        };
        for f in source_fields {
            if fields.contains_key(f.as_str()) {
                return Ok(Some(RowsWriteRejection {
                    code: "backend_read_only_field",
                    location: format!("frontmatter.{f}"),
                    message: format!(
                        "`{f}` is a source column of `{skill}` and read-only; keep your own fields \
                         (for example an assessment) and the body in the companion page instead"
                    ),
                }));
            }
        }
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plain_key_is_its_own_id() {
        assert_eq!(encode_row_id(&["0004500123".into()]), "0004500123");
        assert_eq!(encode_row_id(&["order-4500123".into()]), "order-4500123");
    }

    #[test]
    fn an_unsafe_key_is_encoded_reversibly() {
        for raw in ["a b", "x/y", "ünï", "~", "100%", "a|b#c@d"] {
            let id = encode_row_id(&[raw.to_owned()]);
            assert!(
                id.chars()
                    .all(|c| c.is_ascii_alphanumeric() || "._-~".contains(c)),
                "{id} is a safe id"
            );
            assert_eq!(decode_row_id(&id, 1), Some(vec![raw.to_owned()]), "{raw}");
        }
    }

    #[test]
    fn a_composite_key_joins_with_a_dash_and_round_trips_even_with_dashes_inside() {
        let parts = vec!["4500123".to_owned(), "a-b".to_owned()];
        let id = encode_row_id(&parts);
        assert_eq!(id, "4500123-a~2Db");
        assert_eq!(decode_row_id(&id, 2), Some(parts));
        assert_eq!(decode_row_id("only-one-part-too-many", 2), None);
    }

    #[test]
    fn a_malformed_id_decodes_to_nothing() {
        assert_eq!(decode_row_id("bad~Z", 1), None);
        assert_eq!(decode_row_id("trail~", 1), None);
        assert_eq!(decode_row_id("x", 0), None);
    }

    #[test]
    fn cursors_round_trip() {
        let v = vec!["0004500123".to_owned(), "a b".to_owned()];
        assert_eq!(decode_cursor(&encode_cursor(&v), 2).unwrap(), v);
        assert!(decode_cursor("zz", 1).is_err());
        assert!(decode_cursor("00", 2).is_err());
    }

    #[test]
    fn duckdb_types_map_to_field_kinds() {
        for (ty, kind) in [
            ("VARCHAR", "string"),
            ("BIGINT", "int"),
            ("INTEGER", "int"),
            ("DOUBLE", "float"),
            ("DECIMAL(15,2)", "float"),
            ("DATE", "date"),
            ("TIMESTAMP", "datetime"),
            ("BOOLEAN", "bool"),
            ("STRUCT(a INTEGER)", "string"),
        ] {
            assert_eq!(field_kind_for(ty), kind, "{ty}");
        }
    }

    #[test]
    fn dates_are_iso_strings() {
        assert_eq!(days_to_iso(0), "1970-01-01");
        assert_eq!(days_to_iso(20_704), "2026-09-08");
    }

    #[test]
    fn instance_page_ids_split() {
        assert_eq!(
            split_instance_page_id("markdown/instances/sales-order/0004500123.md"),
            Some(("sales-order", "0004500123"))
        );
        assert_eq!(
            instance_page_id("sales-order", "0004500123"),
            "markdown/instances/sales-order/0004500123.md"
        );
        assert_eq!(
            split_instance_page_id(&instance_page_id("sales-order", "0004500123")),
            Some(("sales-order", "0004500123"))
        );
        assert_eq!(split_instance_page_id("markdown/skills/x.md"), None);
        assert_eq!(split_instance_page_id("markdown/instances/a/b/c.md"), None);
    }

    #[test]
    fn a_sealed_cursor_opens_only_with_its_own_prefix() {
        let token = seal_cursor("r1.", b"[\"a\"]");
        assert!(token.starts_with("r1."));
        assert_eq!(open_cursor("r1.", &token).as_deref(), Some(&b"[\"a\"]"[..]));
        assert_eq!(open_cursor("u1.", &token), None);
        assert_eq!(open_cursor("r1.", "r1.not base64 !!"), None);
        assert_eq!(open_cursor("r1.", "plain"), None);
    }
}
