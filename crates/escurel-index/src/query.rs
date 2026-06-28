//! Stored-query execution: the `run_stored_query` agent tool.
//!
//! A query is a markdown page with `type: instance, skill: query`
//! and frontmatter that declares
//!
//! ```yaml
//! id: customer-churn-trend
//! db: relational              # 'relational' (only one supported today)
//! params:
//!   - {name: customer_id, type: text, required: true}
//!   - {name: from_date,   type: date, required: false, default: '2026-01-01'}
//! sql: |
//!   SELECT page_id, skill FROM pages
//!   WHERE skill = :customer_id AND created_at >= :from_date
//! ```
//!
//! `Indexer::run_stored_query(id, args)` looks up the query
//! instance by slug, validates `args` against the declared params,
//! binds them as positional DuckDB prepared-statement parameters
//! (so SQL injection through arg values is impossible), executes,
//! and returns rows + schema.
//!
//! ## What ships here
//!
//! - `db: relational` — query runs against the indexer's own
//!   DuckDB (the `pages` / `blocks` / `links` tables).
//! - Named-param binding (`:name`) translated to positional `?`
//!   in declared-param order. Unknown names rejected before
//!   dispatch; required-but-missing names rejected before dispatch.
//! - Result projection: each row is a JSON object
//!   `{ column: value }`; values come from DuckDB's `Value` enum.
//!
//! ## What does NOT ship
//!
//! - `db: ext` (DuckLake catalogs attached via `attach_external`).
//!   Errors with `UnsupportedDb` for now; M3 lands attach.
//! - Default param values (the spec lets the query declare a
//!   `default:`; today the caller must supply every required name).
//! - Type-coerce params per the declared `type:` field; current
//!   binding passes the JSON value through DuckDB's `Value`-mapped
//!   types as-is.

use std::sync::LazyLock;

use chrono::{DateTime, NaiveTime, SecondsFormat};
use duckdb::types::{TimeUnit, Value as DuckValue};
use duckdb::{ToSql, params_from_iter};
use regex::Regex;
use thiserror::Error;

use crate::{AclCaller, Indexer, IndexerError};

/// Result of [`Indexer::run_stored_query`].
#[derive(Debug, Clone, PartialEq)]
pub struct StoredQueryResult {
    pub rows: Vec<serde_json::Map<String, serde_json::Value>>,
    pub schema: Vec<ColumnSchema>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColumnSchema {
    pub name: String,
    pub type_name: String,
}

/// Result of [`Indexer::query_instance`] — the full (aggregated) result set
/// of a parameterised read over a `sql_view` instance's managed view, capped
/// at [`MAX_RESULT_ROWS`] with `truncated` set when the cap clipped the tail.
#[derive(Debug, Clone, PartialEq)]
pub struct QueryInstanceResult {
    pub rows: Vec<serde_json::Map<String, serde_json::Value>>,
    pub schema: Vec<ColumnSchema>,
    /// True when the result set hit [`MAX_RESULT_ROWS`] and rows were dropped.
    pub truncated: bool,
}

/// Backstop on the rows [`Indexer::query_instance`] returns. Reports are
/// expected to be aggregated (small), but a misauthored report must never
/// stream an unbounded result set into a single response.
pub const MAX_RESULT_ROWS: usize = 10_000;

#[derive(Debug, Error)]
pub enum QueryError {
    #[error("[[query::{id}]] not found in this tenant")]
    NotFound { id: String },

    #[error("[[query::{id}]] is not a query instance (page exists but is not in `query` skill)")]
    WrongType { id: String },

    #[error("[[query::{id}]] missing required parameter: {name}")]
    MissingParam { id: String, name: String },

    #[error("[[query::{id}]] unknown parameter: {name}")]
    UnknownParam { id: String, name: String },

    #[error("[[query::{id}]] missing `sql` in frontmatter")]
    MissingSql { id: String },

    #[error("[[query::{id}]] declares db = {db:?} but only 'relational' is supported today")]
    UnsupportedDb { id: String, db: String },

    #[error("[[query::{id}]] declares no `target` instance (query_instance requires one)")]
    MissingTarget { id: String },

    #[error("[[query::{id}]] target {target} does not resolve to an instance in this tenant")]
    TargetNotFound { id: String, target: String },

    #[error("[[query::{id}]] target {target} is not a readable sql_view instance")]
    TargetNotSqlView { id: String, target: String },

    #[error("[[query::{id}]] caller is not authorised to read target {target}")]
    Forbidden { id: String, target: String },

    #[error(
        "[[query::{id}]] sql contains an unsupported placeholder {placeholder:?} \
         (only {{{{target}}}} is allowed)"
    )]
    UnknownPlaceholder { id: String, placeholder: String },

    #[error("table {table:?} is not inspectable; allowed: {allowed}")]
    UnknownTable { table: String, allowed: String },

    #[error("duckdb error: {0}")]
    Duckdb(#[from] duckdb::Error),

    #[error("indexer lookup error: {0}")]
    Indexer(#[from] Box<IndexerError>),

    #[error("serde_json error: {0}")]
    Json(#[from] serde_json::Error),
}

/// Matches `:name` placeholders in query SQL, ignoring `::` casts
/// (DuckDB uses `expr::TYPE` for casting; we must not treat the
/// `TYPE` segment as a named param).
///
/// `(?:^|[^:])` ensures the colon is not preceded by another colon.
static NAMED_PARAM_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?:^|[^:]):([A-Za-z_][A-Za-z0-9_]*)").expect("regex"));

impl Indexer {
    /// Look up the `[[query::query_id]]` page, validate `args`
    /// against its declared params, and execute the SQL against
    /// the indexer's DuckDB connection.
    pub async fn run_stored_query(
        &self,
        query_id: &str,
        args: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<StoredQueryResult, QueryError> {
        // 1. Resolve the query page.
        let resolved = self
            .resolve(&format!("[[query::{query_id}]]"), None)
            .await
            .map_err(|err| QueryError::Indexer(Box::new(err)))?;
        let page = resolved.page.ok_or_else(|| QueryError::NotFound {
            id: query_id.to_owned(),
        })?;
        if page.skill != "query" {
            return Err(QueryError::WrongType {
                id: query_id.to_owned(),
            });
        }

        // 2. Re-fetch the page's frontmatter (resolve doesn't include it).
        let fm =
            self.page_frontmatter(&page.page_id)
                .await?
                .ok_or_else(|| QueryError::NotFound {
                    id: query_id.to_owned(),
                })?;

        // 3. Validate `db`.
        let db = fm
            .get("db")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("relational");
        if db != "relational" {
            return Err(QueryError::UnsupportedDb {
                id: query_id.to_owned(),
                db: db.to_owned(),
            });
        }

        // 4. Extract `sql` + the declared params.
        let sql = fm
            .get("sql")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| QueryError::MissingSql {
                id: query_id.to_owned(),
            })?
            .to_owned();
        let declared = declared_params(&fm);

        // 5. Validate args, bind `:name` → positional `?`, and execute.
        // No row cap: a stored query is admin-gated and may legitimately
        // project a large corpus-wide aggregate.
        let conn = self.conn.lock().await;
        let (rows, schema, _truncated) =
            execute_bound_query(&conn, query_id, &sql, &declared, args, None)?;
        Ok(StoredQueryResult { rows, schema })
    }

    /// Execute a `[[query::query_id]]` page that declares a
    /// `target: [[skill::id]]` **against that instance's managed `vw_…`
    /// view**, returning the full (aggregated) result set (issue #205).
    ///
    /// Two trust boundaries are kept separate, by construction:
    ///
    /// - **Identifier position** — the `{{target}}` placeholder is replaced
    ///   with the target's `backend_ref.view`, which is allow-listed through
    ///   [`crate::backend::is_managed_view`] (the deterministic `vw_` prefix).
    ///   It is never a bound value.
    /// - **Value position** — every `:param` runtime value supplied by the
    ///   caller is bound as a positional DuckDB prepared-statement parameter
    ///   (the [`Self::run_stored_query`] pattern). Runtime input never reaches
    ///   the SQL text, so injection through a param value is impossible and it
    ///   never flows through the `sql_view` blocklist-interpolation path.
    ///
    /// The per-instance ACL is applied **fail-closed** to the *target*
    /// instance via [`Indexer::may_read_instance`]: the caller must be allowed
    /// to read the underlying data, not merely the query template.
    pub async fn query_instance(
        &self,
        query_id: &str,
        args: &serde_json::Map<String, serde_json::Value>,
        caller: &AclCaller<'_>,
    ) -> Result<QueryInstanceResult, QueryError> {
        // 1. Resolve the query page and confirm it is a `query` instance.
        let resolved = self
            .resolve(&format!("[[query::{query_id}]]"), None)
            .await
            .map_err(|err| QueryError::Indexer(Box::new(err)))?;
        let page = resolved.page.ok_or_else(|| QueryError::NotFound {
            id: query_id.to_owned(),
        })?;
        if page.skill != "query" {
            return Err(QueryError::WrongType {
                id: query_id.to_owned(),
            });
        }

        // 2. Read its frontmatter: the `target` ref, the `sql`, the params.
        let fm =
            self.page_frontmatter(&page.page_id)
                .await?
                .ok_or_else(|| QueryError::NotFound {
                    id: query_id.to_owned(),
                })?;
        let target_raw = fm
            .get("target")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| QueryError::MissingTarget {
                id: query_id.to_owned(),
            })?;
        let target_link =
            crate::read::first_wikilink_target(target_raw).unwrap_or_else(|| target_raw.to_owned());
        let sql = fm
            .get("sql")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| QueryError::MissingSql {
                id: query_id.to_owned(),
            })?
            .to_owned();
        let declared = declared_params(&fm);

        // 3. Resolve + expand the target instance (its frontmatter carries the
        //    managed view name and is the input to the ACL decision).
        let target_page = self
            .resolve(&target_link, None)
            .await
            .map_err(|err| QueryError::Indexer(Box::new(err)))?
            .page
            .ok_or_else(|| QueryError::TargetNotFound {
                id: query_id.to_owned(),
                target: target_link.clone(),
            })?;
        let expanded = self
            .expand(&target_page.page_id, None, None)
            .await
            .map_err(|err| QueryError::Indexer(Box::new(err)))?
            .ok_or_else(|| QueryError::TargetNotFound {
                id: query_id.to_owned(),
                target: target_link.clone(),
            })?;

        // 4. ACL — fail closed on the TARGET instance (read the data, not just
        //    the template). Admin bypasses inside `may_read_instance`.
        if !self
            .may_read_instance(caller, &expanded.page.skill, &expanded.frontmatter)
            .await
            .map_err(|err| QueryError::Indexer(Box::new(err)))?
        {
            return Err(QueryError::Forbidden {
                id: query_id.to_owned(),
                target: target_link,
            });
        }

        // 5. Recover + allow-list the managed view (`vw_…`). A target that is
        //    not a readable sql_view instance — or a tampered backend_ref that
        //    points at a server-side table — is refused here.
        let view = expanded
            .frontmatter
            .get("backend_ref")
            .filter(|b| b.get("kind").and_then(serde_json::Value::as_str) == Some("sql_view"))
            .and_then(|b| b.get("view"))
            .and_then(serde_json::Value::as_str)
            .filter(|v| crate::backend::is_managed_view(v))
            .ok_or_else(|| QueryError::TargetNotSqlView {
                id: query_id.to_owned(),
                target: target_link,
            })?;

        // 6. Splice the allow-listed view into the `{{target}}` placeholder,
        //    then bind runtime `:param` values and execute (row-capped).
        let sql = substitute_target(&sql, view, query_id)?;
        let conn = self.conn.lock().await;
        let (rows, schema, truncated) = execute_bound_query(
            &conn,
            query_id,
            &sql,
            &declared,
            args,
            Some(MAX_RESULT_ROWS),
        )?;
        Ok(QueryInstanceResult {
            rows,
            schema,
            truncated,
        })
    }

    /// The frontmatter object of an indexed page, or `None` when no such page
    /// exists. Shared by `run_stored_query` / `query_instance`; takes and
    /// releases the connection lock so the caller can re-lock for execution.
    async fn page_frontmatter(
        &self,
        page_id: &str,
    ) -> Result<Option<serde_json::Value>, QueryError> {
        let conn = self.conn.lock().await;
        let fm_json: Option<String> = conn
            .query_row(
                "SELECT frontmatter::VARCHAR FROM pages WHERE page_id = ?",
                duckdb::params![page_id],
                |row| row.get(0),
            )
            .ok();
        drop(conn);
        match fm_json {
            Some(j) => Ok(Some(serde_json::from_str(&j)?)),
            None => Ok(None),
        }
    }

    /// Read up to `limit` rows from an allow-listed index table for
    /// operator inspection (the `admin_index_query` admin tool).
    ///
    /// This is deliberately **not** arbitrary SQL: `table` must be one
    /// of [`INSPECTABLE_TABLES`] (the name is spliced as a literal only
    /// after that check, so there is no injection surface), and `limit`
    /// is clamped to `[1, 1000]`. The heavy `dense_vec FLOAT[768]`
    /// column is excluded from the vector-bearing tables so the JSON
    /// projection stays small. Returns the same shape as
    /// [`Self::run_stored_query`].
    pub async fn inspect_table(
        &self,
        table: &str,
        limit: usize,
    ) -> Result<StoredQueryResult, QueryError> {
        if !INSPECTABLE_TABLES.contains(&table) {
            return Err(QueryError::UnknownTable {
                table: table.to_owned(),
                allowed: INSPECTABLE_TABLES.join(", "),
            });
        }
        let limit = limit.clamp(1, 1000);
        // `dense_vec` is a 768-float array; exclude it from the
        // tables that carry it so an inspector row isn't ~6 KB of
        // numbers. Table name is allow-listed above, so this literal
        // splice is safe.
        let projection = if matches!(table, "blocks" | "chat_messages") {
            "* EXCLUDE (dense_vec)"
        } else {
            "*"
        };
        let sql = format!("SELECT {projection} FROM {table} LIMIT {limit}");

        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(&sql)?;
        let mut rows_iter = stmt.query([])?;

        let (col_names, schema): (Vec<String>, Vec<ColumnSchema>) = match rows_iter.as_ref() {
            None => (Vec::new(), Vec::new()),
            Some(s) => {
                let n = s.column_count();
                let names: Vec<String> = (0..n)
                    .map(|i| s.column_name(i).map(ToOwned::to_owned).unwrap_or_default())
                    .collect();
                let schema = (0..n)
                    .map(|i| ColumnSchema {
                        name: names[i].clone(),
                        type_name: format!("{:?}", s.column_type(i)),
                    })
                    .collect();
                (names, schema)
            }
        };

        let mut rows = Vec::new();
        while let Some(row) = rows_iter.next()? {
            let mut obj = serde_json::Map::new();
            for (i, name) in col_names.iter().enumerate() {
                let v: DuckValue = row.get(i)?;
                obj.insert(name.clone(), duck_to_json(v));
            }
            rows.push(obj);
        }
        Ok(StoredQueryResult { rows, schema })
    }
}

/// Index tables an operator may read via `admin_index_query`. No
/// arbitrary SQL — [`Indexer::inspect_table`] matches against this
/// fixed set before touching the database.
pub const INSPECTABLE_TABLES: &[&str] = &[
    "pages",
    "blocks",
    "links",
    "crdt_ops",
    "crdt_snapshots",
    "chat_messages",
];

#[derive(Debug, Clone)]
struct DeclaredParam {
    name: String,
    required: bool,
}

fn declared_params(fm: &serde_json::Value) -> Vec<DeclaredParam> {
    fm.get("params")
        .and_then(serde_json::Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|item| {
                    let name = item.get("name").and_then(|n| n.as_str())?.to_owned();
                    let required = item
                        .get("required")
                        .and_then(serde_json::Value::as_bool)
                        .unwrap_or(false);
                    Some(DeclaredParam { name, required })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Validate `args` against the declared params, rewrite `:name` → positional
/// `?`, bind each value as a DuckDB prepared-statement parameter, and execute
/// against `conn`. Returns the rows, the result schema, and whether `row_cap`
/// clipped the tail. Shared by [`Indexer::run_stored_query`] (no cap) and
/// [`Indexer::query_instance`] (capped at [`MAX_RESULT_ROWS`]).
///
/// Runtime values flow ONLY through the positional-bind path here — they are
/// never spliced into the SQL text — so injection through a param value is
/// impossible regardless of caller (issue #205).
///
/// Returns `(rows, schema, truncated)` — see [`ExecutedRows`].
type ExecutedRows = (
    Vec<serde_json::Map<String, serde_json::Value>>,
    Vec<ColumnSchema>,
    bool,
);
fn execute_bound_query(
    conn: &duckdb::Connection,
    id: &str,
    sql: &str,
    declared: &[DeclaredParam],
    args: &serde_json::Map<String, serde_json::Value>,
    row_cap: Option<usize>,
) -> Result<ExecutedRows, QueryError> {
    // Unknown args and missing required args are rejected before any DB call.
    for (name, _val) in args {
        if !declared.iter().any(|p| &p.name == name) {
            return Err(QueryError::UnknownParam {
                id: id.to_owned(),
                name: name.clone(),
            });
        }
    }
    for p in declared {
        if p.required && !args.contains_key(&p.name) {
            return Err(QueryError::MissingParam {
                id: id.to_owned(),
                name: p.name.clone(),
            });
        }
    }

    // Substitute `:name` → `?` and build the positional value vector.
    let (rewritten_sql, bind_order) = rewrite_named_params(sql);
    let mut bound_values: Vec<DuckValue> = Vec::with_capacity(bind_order.len());
    for name in &bind_order {
        let v = args.get(name).cloned().unwrap_or(serde_json::Value::Null);
        bound_values.push(json_to_duck(v));
    }

    // Execute. Column metadata is only available after query() in duckdb-rs
    // (prepare alone leaves the statement un-executed; column_count() before
    // query panics with "statement not executed yet").
    let mut stmt = conn.prepare(&rewritten_sql)?;
    let params_refs: Vec<&dyn ToSql> = bound_values.iter().map(|v| v as &dyn ToSql).collect();
    let mut rows_iter = stmt.query(params_from_iter(params_refs.iter()))?;

    let (col_names, col_type_names): (Vec<String>, Vec<String>) = match rows_iter.as_ref() {
        None => (Vec::new(), Vec::new()),
        Some(s) => {
            let n = s.column_count();
            let names: Vec<String> = (0..n)
                .map(|i| s.column_name(i).map(|v| v.to_string()).unwrap_or_default())
                .collect();
            let types: Vec<String> = (0..n).map(|i| format!("{:?}", s.column_type(i))).collect();
            (names, types)
        }
    };
    let schema: Vec<ColumnSchema> = col_names
        .iter()
        .zip(col_type_names.iter())
        .map(|(name, ty)| ColumnSchema {
            name: name.clone(),
            type_name: ty.clone(),
        })
        .collect();

    let mut rows = Vec::new();
    let mut truncated = false;
    while let Some(row) = rows_iter.next()? {
        if let Some(cap) = row_cap
            && rows.len() >= cap
        {
            truncated = true;
            break;
        }
        let mut obj = serde_json::Map::new();
        for (i, name) in col_names.iter().enumerate() {
            let v: DuckValue = row.get(i)?;
            obj.insert(name.clone(), duck_to_json(v));
        }
        rows.push(obj);
    }

    Ok((rows, schema, truncated))
}

/// Replace the `{{target}}` placeholder with the allow-listed `view`
/// identifier. `view` MUST already be validated by
/// [`crate::backend::is_managed_view`] (the caller does this) — it is the only
/// identifier-position substitution permitted, so any other `{{…}}` token is
/// rejected rather than silently left in the SQL.
fn substitute_target(sql: &str, view: &str, id: &str) -> Result<String, QueryError> {
    let replaced = sql.replace("{{target}}", view);
    if let Some(pos) = replaced.find("{{") {
        let placeholder: String = replaced[pos..].chars().take(32).collect();
        return Err(QueryError::UnknownPlaceholder {
            id: id.to_owned(),
            placeholder,
        });
    }
    Ok(replaced)
}

/// Rewrite `:name` placeholders in `sql` to positional `?` and
/// return the sequence of names in bind order.
fn rewrite_named_params(sql: &str) -> (String, Vec<String>) {
    let mut bind_order = Vec::new();
    let rewritten = NAMED_PARAM_RE.replace_all(sql, |caps: &regex::Captures| {
        let name = caps[1].to_owned();
        bind_order.push(name);
        // Preserve the character that wasn't the colon (the regex
        // captured `[^:]:` so we re-emit the prefix char + `?`).
        let prefix = caps.get(0).unwrap().as_str();
        let prefix_char = if prefix.starts_with(':') {
            String::new()
        } else {
            prefix.chars().next().unwrap().to_string()
        };
        format!("{prefix_char}?")
    });
    (rewritten.into_owned(), bind_order)
}

fn json_to_duck(v: serde_json::Value) -> DuckValue {
    match v {
        serde_json::Value::Null => DuckValue::Null,
        serde_json::Value::Bool(b) => DuckValue::Boolean(b),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                DuckValue::BigInt(i)
            } else if let Some(f) = n.as_f64() {
                DuckValue::Double(f)
            } else {
                DuckValue::Text(n.to_string())
            }
        }
        serde_json::Value::String(s) => DuckValue::Text(s),
        // Arrays and objects: best-effort coercion to their JSON
        // string. The spec's `params:` schema doesn't support
        // composite types today.
        other => DuckValue::Text(other.to_string()),
    }
}

fn duck_to_json(v: DuckValue) -> serde_json::Value {
    use serde_json::{Number, Value};
    match v {
        DuckValue::Null => Value::Null,
        DuckValue::Boolean(b) => Value::Bool(b),
        DuckValue::TinyInt(n) => Value::Number(Number::from(n)),
        DuckValue::SmallInt(n) => Value::Number(Number::from(n)),
        DuckValue::Int(n) => Value::Number(Number::from(n)),
        DuckValue::BigInt(n) => Value::Number(Number::from(n)),
        DuckValue::HugeInt(n) => Value::String(n.to_string()),
        DuckValue::UTinyInt(n) => Value::Number(Number::from(n)),
        DuckValue::USmallInt(n) => Value::Number(Number::from(n)),
        DuckValue::UInt(n) => Value::Number(Number::from(n)),
        DuckValue::UBigInt(n) => Value::Number(Number::from(n)),
        DuckValue::Float(f) => Number::from_f64(f64::from(f))
            .map(Value::Number)
            .unwrap_or(Value::Null),
        DuckValue::Double(f) => Number::from_f64(f)
            .map(Value::Number)
            .unwrap_or(Value::Null),
        DuckValue::Text(s) => Value::String(s),
        DuckValue::Blob(b) => Value::String(format!("0x{}", hex_str(&b))),
        // Temporal types render as ISO-8601 / RFC-3339 strings so
        // consumers get usable, sortable, Vega-friendly values rather
        // than the Rust `Debug` form (`"Date32(9862)"`). See issue #211.
        DuckValue::Date32(days) => date_to_json(days),
        DuckValue::Timestamp(unit, v) => timestamp_to_json(unit, v),
        DuckValue::Time64(unit, v) => time_to_json(unit, v),
        DuckValue::Interval {
            months,
            days,
            nanos,
        } => Value::String(interval_to_iso8601(months, days, nanos)),
        // Remaining composite/exotic types (List, Struct, Map, …):
        // render via Debug for now.
        other => Value::String(format!("{other:?}")),
    }
}

/// Split a count of `unit`s into whole seconds + leftover nanoseconds,
/// using Euclidean division so values before the epoch (negative `v`)
/// still yield a non-negative nanosecond remainder.
fn unit_to_secs_nanos(unit: TimeUnit, v: i64) -> (i64, u32) {
    let (per_sec, nanos_per): (i64, i64) = match unit {
        TimeUnit::Second => (1, 1_000_000_000),
        TimeUnit::Millisecond => (1_000, 1_000_000),
        TimeUnit::Microsecond => (1_000_000, 1_000),
        TimeUnit::Nanosecond => (1_000_000_000, 1),
    };
    let secs = v.div_euclid(per_sec);
    let nanos = (v.rem_euclid(per_sec) * nanos_per) as u32;
    (secs, nanos)
}

/// `DATE` → `"YYYY-MM-DD"` (days since the Unix epoch).
fn date_to_json(days: i32) -> serde_json::Value {
    match DateTime::from_timestamp(i64::from(days) * 86_400, 0) {
        Some(dt) => serde_json::Value::String(dt.format("%Y-%m-%d").to_string()),
        None => serde_json::Value::String(format!("Date32({days})")),
    }
}

/// `TIMESTAMP` → `"YYYY-MM-DDTHH:MM:SS[.fraction]Z"` (RFC-3339, UTC).
fn timestamp_to_json(unit: TimeUnit, v: i64) -> serde_json::Value {
    let (secs, nanos) = unit_to_secs_nanos(unit, v);
    match DateTime::from_timestamp(secs, nanos) {
        Some(dt) => serde_json::Value::String(dt.to_rfc3339_opts(SecondsFormat::AutoSi, true)),
        None => serde_json::Value::String(format!("Timestamp({unit:?}, {v})")),
    }
}

/// `TIME` → `"HH:MM:SS[.fraction]"` (time of day).
fn time_to_json(unit: TimeUnit, v: i64) -> serde_json::Value {
    let (secs, nanos) = unit_to_secs_nanos(unit, v);
    match u32::try_from(secs)
        .ok()
        .and_then(|s| NaiveTime::from_num_seconds_from_midnight_opt(s, nanos))
    {
        Some(t) => serde_json::Value::String(t.format("%H:%M:%S%.f").to_string()),
        None => serde_json::Value::String(format!("Time64({unit:?}, {v})")),
    }
}

/// `INTERVAL` → an ISO-8601 duration string (`P[n]M[n]DT[n]S`). DuckDB
/// keeps months, days and sub-day nanoseconds as independent fields
/// (months/days are not normalised to seconds), so each maps to its own
/// ISO component. The zero interval renders as `"PT0S"`.
fn interval_to_iso8601(months: i32, days: i32, nanos: i64) -> String {
    let mut out = String::from("P");
    if months != 0 {
        out.push_str(&format!("{months}M"));
    }
    if days != 0 {
        out.push_str(&format!("{days}D"));
    }
    let secs = nanos / 1_000_000_000;
    let frac = (nanos % 1_000_000_000).abs();
    if nanos != 0 || out == "P" {
        out.push('T');
        if frac != 0 {
            // Trim trailing zeros from the fractional-second part.
            let frac_str = format!("{frac:09}");
            out.push_str(&format!("{secs}.{}S", frac_str.trim_end_matches('0')));
        } else {
            out.push_str(&format!("{secs}S"));
        }
    }
    out
}

fn hex_str(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0xf) as usize] as char);
    }
    out
}

#[cfg(test)]
mod temporal_tests {
    use super::*;
    use serde_json::Value;

    fn s(v: Value) -> String {
        match v {
            Value::String(s) => s,
            other => panic!("expected string, got {other:?}"),
        }
    }

    #[test]
    fn date_renders_iso_including_pre_epoch() {
        // 9862 days after 1970-01-01 is the issue's repro value.
        assert_eq!(s(date_to_json(9862)), "1997-01-01");
        assert_eq!(s(date_to_json(0)), "1970-01-01");
        // Negative day counts (before the epoch) must not underflow.
        assert_eq!(s(date_to_json(-1)), "1969-12-31");
    }

    #[test]
    fn timestamp_renders_rfc3339_utc_with_z() {
        // Whole-second timestamp → no fractional part, trailing `Z`.
        // 1997-01-01T13:45:30Z = 9862*86400 + 49530 seconds.
        assert_eq!(
            s(timestamp_to_json(TimeUnit::Second, 852_126_330)),
            "1997-01-01T13:45:30Z",
        );
        // Microsecond unit, sub-second value preserved.
        assert_eq!(
            s(timestamp_to_json(
                TimeUnit::Microsecond,
                852_126_330_500_000
            )),
            "1997-01-01T13:45:30.500Z",
        );
    }

    #[test]
    fn time_renders_iso_time_of_day() {
        // 13:45:30 in microseconds since midnight.
        let micros = (13 * 3600 + 45 * 60 + 30) * 1_000_000;
        assert_eq!(s(time_to_json(TimeUnit::Microsecond, micros)), "13:45:30");
        assert_eq!(s(time_to_json(TimeUnit::Second, 0)), "00:00:00");
    }

    #[test]
    fn interval_renders_iso8601_duration() {
        // 1 month, 2 days, 03:04:05 → independent ISO components.
        let nanos = ((3 * 3600 + 4 * 60 + 5) as i64) * 1_000_000_000;
        assert_eq!(interval_to_iso8601(1, 2, nanos), "P1M2DT11045S");
        // The zero interval is the canonical "PT0S".
        assert_eq!(interval_to_iso8601(0, 0, 0), "PT0S");
        // Months/days only — no time component.
        assert_eq!(interval_to_iso8601(14, 0, 0), "P14M");
        // Sub-second fraction, trailing zeros trimmed.
        assert_eq!(interval_to_iso8601(0, 0, 500_000_000), "PT0.5S");
    }
}
