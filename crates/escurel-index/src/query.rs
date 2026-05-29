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

use duckdb::types::Value as DuckValue;
use duckdb::{ToSql, params_from_iter};
use regex::Regex;
use thiserror::Error;

use crate::{Indexer, IndexerError};

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

        // 2. Re-fetch the page's frontmatter (resolve doesn't include
        // it).
        let conn = self.conn.lock().await;
        let fm_json: String = conn
            .query_row(
                "SELECT frontmatter::VARCHAR FROM pages WHERE page_id = ?",
                duckdb::params![page.page_id],
                |row| row.get(0),
            )
            .map_err(|_| QueryError::NotFound {
                id: query_id.to_owned(),
            })?;
        let fm: serde_json::Value = serde_json::from_str(&fm_json)?;

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

        // 4. Extract `sql`.
        let sql = fm
            .get("sql")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| QueryError::MissingSql {
                id: query_id.to_owned(),
            })?
            .to_owned();

        // 5. Extract declared param names (the order in the `params:`
        // array). Unknown args and missing required args are rejected
        // here before any DB call.
        let declared = declared_params(&fm);
        for (name, _val) in args {
            if !declared.iter().any(|p| &p.name == name) {
                return Err(QueryError::UnknownParam {
                    id: query_id.to_owned(),
                    name: name.clone(),
                });
            }
        }
        for p in &declared {
            if p.required && !args.contains_key(&p.name) {
                return Err(QueryError::MissingParam {
                    id: query_id.to_owned(),
                    name: p.name.clone(),
                });
            }
        }

        // 6. Substitute `:name` → `?` and build positional bind order.
        let (rewritten_sql, bind_order) = rewrite_named_params(&sql);

        // 7. Build the positional value vector in bind_order.
        let mut bound_values: Vec<DuckValue> = Vec::with_capacity(bind_order.len());
        for name in &bind_order {
            let v = args.get(name).cloned().unwrap_or(serde_json::Value::Null);
            bound_values.push(json_to_duck(v));
        }

        // 8. Execute. Column metadata is only available after
        // query() in duckdb-rs (prepare alone leaves the statement
        // un-executed; calling column_count() before query panics
        // with "statement not executed yet").
        let mut stmt = conn.prepare(&rewritten_sql)?;
        let params_refs: Vec<&dyn ToSql> = bound_values.iter().map(|v| v as &dyn ToSql).collect();
        let mut rows_iter = stmt.query(params_from_iter(params_refs.iter()))?;

        let (col_count, col_names, col_type_names): (usize, Vec<String>, Vec<String>) =
            match rows_iter.as_ref() {
                None => (0, Vec::new(), Vec::new()),
                Some(s) => {
                    let n = s.column_count();
                    let names: Vec<String> = (0..n)
                        .map(|i| s.column_name(i).map(|v| v.to_string()).unwrap_or_default())
                        .collect();
                    let types: Vec<String> =
                        (0..n).map(|i| format!("{:?}", s.column_type(i))).collect();
                    (n, names, types)
                }
            };
        let _ = col_count;
        let schema: Vec<ColumnSchema> = col_names
            .iter()
            .zip(col_type_names.iter())
            .map(|(name, ty)| ColumnSchema {
                name: name.clone(),
                type_name: ty.clone(),
            })
            .collect();

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
    "frontmatter_index",
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
        // Date/time/etc.: render via Debug for now (RFC 3339 round-
        // trip is a separate concern handled at the API layer).
        other => Value::String(format!("{other:?}")),
    }
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
