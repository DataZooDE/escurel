//! Write-back to a row of a database-backed `rows` skill (stage 4c, SQL).
//!
//! Reads go through the indexer's persistent connection and a READ_ONLY attachment. A write never
//! does: it opens its OWN short-lived DuckDB connection, attaches the source READ-WRITE, and applies
//! ONE `UPDATE` inside ONE transaction:
//!
//! ```sql
//! UPDATE src.schema.table SET "col" = CAST(? AS <type>), ...
//!  WHERE CAST("key" AS VARCHAR) = ?                      -- the row, by its key
//!    AND CAST("c1" AS VARCHAR) IS NOT DISTINCT FROM ?    -- ...and ONLY if it still is what the
//!    AND CAST("c2" AS VARCHAR) IS NOT DISTINCT FROM ?    --    proposer's reviewer saw (every column)
//! ```
//!
//! Every value is a BOUND parameter; every identifier comes from `DESCRIBE` of the live relation (never
//! from the caller) and is double-quoted; the cast type is DuckDB's own type name for the column. An
//! `UPDATE` that matches no row is a CONFLICT (the row changed or vanished since the basis was read),
//! never a silent success. The check and the write share one transaction on the source, so nothing
//! can slip between them.

use std::time::Duration;

use serde_json::Value;

use super::binding::SqlConnector;
use super::rows::{RowRecord, RowsSource, decode_row_id};
use super::sql_view::{
    SqlViewError, attach_sql_rw, describe, install_load, is_valid_db_relation, is_valid_identifier,
    resolve_attach,
};
use crate::Indexer;

/// Why a row write did not happen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RowWriteError {
    /// The source's connector cannot be written (`json_dir`, `parquet_dir`, …) or the field is not
    /// writable. Nothing was attempted.
    NotWritable(String),
    /// No row matched the key AND the basis: it changed, or disappeared, since it was read.
    Conflict,
    /// The source could not be reached or was busy (connection refused, timeout, locked). Safe to try
    /// again: the transaction was rolled back.
    Transient(String),
    /// The source refused the change (a constraint, a type error, more than one row matched). Trying
    /// again will not help.
    Final(String),
}

impl std::fmt::Display for RowWriteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotWritable(m) | Self::Transient(m) | Self::Final(m) => f.write_str(m),
            Self::Conflict => f.write_str("the row changed or disappeared since it was read"),
        }
    }
}

/// A scalar patch value as the text a `CAST(? AS <type>)` takes.
fn param_text(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

/// A DuckDB type name is spliced into `CAST(.. AS <type>)`: only the shapes DuckDB itself reports
/// (`VARCHAR`, `DECIMAL(18,3)`, `TIMESTAMP WITH TIME ZONE`, …) pass.
fn safe_type(t: &str) -> bool {
    !t.is_empty()
        && t.len() <= 64
        && t.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '(' | ')' | ',' | ' '))
}

/// Whether a source error is the kind a retry can fix (a connection or a lock), as opposed to a
/// refusal by the data model.
fn classify(msg: &str) -> bool {
    let m = msg.to_ascii_lowercase();
    [
        "could not connect",
        "connection refused",
        "connection reset",
        "connection closed",
        "connection to server",
        "server closed the connection",
        "terminating connection",
        "timeout",
        "timed out",
        "database is locked",
        "database table is locked",
        "busy",
        "no route to host",
        "network is unreachable",
        "too many connections",
        "the database system is starting up",
        "interrupted",
    ]
    .iter()
    .any(|k| m.contains(k))
}

/// The source's own words, with the connection string removed (it may carry a password).
fn scrub(msg: &str, secret: &str) -> String {
    crate::dsn::scrub(msg, secret).chars().take(200).collect()
}

impl Indexer {
    /// Whether this `rows` source can be written back at all: a database connector (not a file
    /// directory) and at least one writable column.
    #[must_use]
    pub fn rows_source_is_writable(src: &RowsSource) -> bool {
        matches!(
            src.sql.connector,
            SqlConnector::Postgres | SqlConnector::Mysql | SqlConnector::Sqlite
        ) && !src.cfg.writable_columns.is_empty()
    }

    /// The source column behind a frontmatter field name of this skill (`None` when the field is not
    /// a projected column).
    #[must_use]
    pub fn rows_column_for_field(src: &RowsSource, field: &str) -> Option<String> {
        src.project
            .iter()
            .find(|(_, f)| f.as_str() == field)
            .map(|(c, _)| c.clone())
    }

    /// Apply `patch` (frontmatter field name -> scalar) to the row `id`, only if the row still holds
    /// the values of `basis` (the [`RowRecord::source_texts`] read just before). See the module docs.
    ///
    /// # Errors
    /// [`RowWriteError`]: `NotWritable` (nothing attempted), `Conflict` (rolled back), `Transient`
    /// (rolled back, retry), `Final` (rolled back).
    pub async fn rows_apply_patch(
        &self,
        src: &RowsSource,
        id: &str,
        basis: &RowRecord,
        patch: &serde_json::Map<String, Value>,
    ) -> Result<(), RowWriteError> {
        if !Self::rows_source_is_writable(src) {
            return Err(RowWriteError::NotWritable(format!(
                "the `{}` source is read-only (a `{}` connector, writable columns {:?})",
                src.skill,
                src.sql.connector.as_str(),
                src.cfg.writable_columns
            )));
        }
        let mut sets: Vec<(String, String)> = Vec::new();
        for (field, value) in patch {
            if !src.cfg.writable_columns.contains(field) {
                return Err(RowWriteError::NotWritable(format!(
                    "`{field}` is not a writable column of `{}`",
                    src.skill
                )));
            }
            let Some(column) = Self::rows_column_for_field(src, field) else {
                return Err(RowWriteError::NotWritable(format!(
                    "`{field}` is not a projected column of `{}`",
                    src.skill
                )));
            };
            let Some(text) = param_text(value) else {
                return Err(RowWriteError::NotWritable(format!(
                    "`{field}` must be a string, number or boolean"
                )));
            };
            sets.push((column, text));
        }
        let Some(key_values) = decode_row_id(id, src.cfg.key.len()) else {
            return Err(RowWriteError::Final(format!("`{id}` is not a row id")));
        };
        let (alias, secret) = resolve_attach(self, &src.sql)
            .await
            .map_err(|e| RowWriteError::Transient(source_unavailable(&e)))?;
        let job = Job {
            connector: src.sql.connector,
            alias,
            secret,
            relation: src.sql.relation.clone(),
            keys: src.cfg.key.clone(),
            key_values,
            basis: basis.source_texts.clone(),
            sets,
            timeout: self.rows_query_timeout,
        };
        tokio::task::spawn_blocking(move || job.run())
            .await
            .map_err(|e| {
                RowWriteError::Transient(format!("the write task did not complete: {e}"))
            })?
    }
}

fn source_unavailable(e: &SqlViewError) -> String {
    format!("the source is not available: {e}")
}

/// Everything the blocking write needs, owned.
struct Job {
    connector: SqlConnector,
    alias: String,
    secret: String,
    relation: String,
    keys: Vec<String>,
    key_values: Vec<String>,
    basis: Vec<(String, Option<String>)>,
    sets: Vec<(String, String)>,
    timeout: Duration,
}

impl Job {
    fn run(self) -> Result<(), RowWriteError> {
        let secret = self.secret.clone();
        let fail = |e: SqlViewError| {
            let m = scrub(&e.to_string(), &secret);
            if classify(&m) {
                RowWriteError::Transient(m)
            } else {
                RowWriteError::Final(m)
            }
        };
        let conn = duckdb::Connection::open_in_memory().map_err(|e| fail(e.into()))?;
        for stmt in install_load(self.connector) {
            conn.execute_batch(stmt).map_err(|e| fail(e.into()))?;
        }
        conn.execute_batch(&attach_sql_rw(self.connector, &self.alias, &self.secret))
            .map_err(|e| fail(e.into()))?;
        super::rows::with_statement_timeout(&conn, self.timeout, || self.apply(&conn)).map_err(
            |e| match e {
                Outcome::Conflict => RowWriteError::Conflict,
                Outcome::Source(e) => fail(e),
                Outcome::Final(m) => RowWriteError::Final(m),
            },
        )
    }

    fn apply(&self, conn: &duckdb::Connection) -> Result<(), Outcome> {
        if !is_valid_db_relation(&self.relation) || !is_valid_identifier(&self.alias) {
            return Err(Outcome::Final(
                "relation or attach name is unsafe".to_owned(),
            ));
        }
        let target = format!("{}.{}", self.alias, self.relation);
        // The live schema: every identifier below must be one of these, and each cast type is one of
        // DuckDB's own names for them.
        let cols = describe(conn, &target).map_err(Outcome::Source)?;
        let ty_of = |name: &str| {
            cols.iter()
                .find(|(n, _)| n == name)
                .map(|(_, t)| t.as_str())
        };
        let mut params: Vec<Option<String>> = Vec::new();
        let mut set_sql = Vec::new();
        for (col, text) in &self.sets {
            let Some(ty) = ty_of(col).filter(|t| safe_type(t)) else {
                return Err(Outcome::Final(format!(
                    "`{col}` is not a column of the source relation any more"
                )));
            };
            set_sql.push(format!("\"{col}\" = CAST(? AS {ty})"));
            params.push(Some(text.clone()));
        }
        let mut where_sql = Vec::new();
        for (key, value) in self.keys.iter().zip(&self.key_values) {
            if ty_of(key).is_none() {
                return Err(Outcome::Final(format!(
                    "key column `{key}` is not in the source"
                )));
            }
            where_sql.push(format!("CAST(\"{key}\" AS VARCHAR) = ?"));
            params.push(Some(value.clone()));
        }
        for (col, text) in &self.basis {
            if ty_of(col).is_none() {
                // The source's shape moved since the row was read: that is a conflict, not a guess.
                return Err(Outcome::Conflict);
            }
            where_sql.push(format!("CAST(\"{col}\" AS VARCHAR) IS NOT DISTINCT FROM ?"));
            params.push(text.clone());
        }
        let sql = format!(
            "UPDATE {target} SET {} WHERE {}",
            set_sql.join(", "),
            where_sql.join(" AND ")
        );
        conn.execute_batch("BEGIN")
            .map_err(|e| Outcome::Source(e.into()))?;
        let changed = match conn.execute(&sql, duckdb::params_from_iter(params.iter())) {
            Ok(n) => n,
            Err(e) => {
                let _ = conn.execute_batch("ROLLBACK");
                return Err(Outcome::Source(e.into()));
            }
        };
        match changed {
            1 => conn
                .execute_batch("COMMIT")
                .map_err(|e| Outcome::Source(e.into())),
            0 => {
                let _ = conn.execute_batch("ROLLBACK");
                Err(Outcome::Conflict)
            }
            n => {
                let _ = conn.execute_batch("ROLLBACK");
                Err(Outcome::Final(format!(
                    "the update matched {n} rows, so the key does not identify one row; nothing was changed"
                )))
            }
        }
    }
}

enum Outcome {
    Conflict,
    Source(SqlViewError),
    Final(String),
}

impl From<SqlViewError> for Outcome {
    fn from(e: SqlViewError) -> Self {
        Self::Source(e)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_duckdb_type_names_are_spliced_into_a_cast() {
        for ok in [
            "VARCHAR",
            "DECIMAL(18,3)",
            "TIMESTAMP WITH TIME ZONE",
            "INTEGER",
        ] {
            assert!(safe_type(ok), "{ok}");
        }
        for bad in ["", "VARCHAR); DROP TABLE x; --", "a'b", "x\"y", "int;"] {
            assert!(!safe_type(bad), "{bad}");
        }
    }

    #[test]
    fn connection_and_lock_errors_retry_and_data_errors_do_not() {
        assert!(classify(
            "IO Error: could not connect to server: Connection refused"
        ));
        assert!(classify("SQLite error: database is locked"));
        assert!(!classify("violates not-null constraint"));
        assert!(!classify(
            "Conversion Error: Could not convert string 'x' to INT32"
        ));
    }

    #[test]
    fn a_connection_string_never_survives_in_an_error() {
        let m = scrub(
            "failed: host=db password=hunter2 dbname=x",
            "host=db password=hunter2 dbname=x",
        );
        assert!(!m.contains("hunter2"), "{m}");
    }

    #[test]
    fn patch_values_become_cast_text() {
        assert_eq!(param_text(&serde_json::json!("a")).as_deref(), Some("a"));
        assert_eq!(param_text(&serde_json::json!(12)).as_deref(), Some("12"));
        assert_eq!(
            param_text(&serde_json::json!(true)).as_deref(),
            Some("true")
        );
        assert_eq!(param_text(&serde_json::json!(null)), None);
        assert_eq!(param_text(&serde_json::json!([1])), None);
    }
}
