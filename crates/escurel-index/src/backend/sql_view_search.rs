//! The SQL lane's local search projection.
//!
//! `search` matches `q` against the `search_text` columns of every `sql_view`
//! instance. Those columns live in the EXTERNAL source behind the view (in
//! production: parquet on object storage). The lane used to run
//! `SELECT count(*) FROM vw_… WHERE col ILIKE ?` per instance, per query
//! variant, on every search — a full remote scan per view, serialised behind the
//! tenant's one connection. In the lab an unfiltered search took more than 90 s,
//! and every other tool on the tenant queued behind it for those scans.
//!
//! Instead, the distinct search texts of each view (with their row counts) are
//! captured into a LOCAL table when the view is created, rebuilt or refreshed,
//! and the lane reads only that table. The capture runs on a cloned connection —
//! the same database instance, so the same views, attachments and secrets — so
//! the source scan does not hold the shared connection.
//!
//! A projection is a snapshot: a change to the source is visible after the next
//! [`crate::Indexer::refresh_search_projections`] (a re-create, a rebuild, or a
//! writer boot). It is a derived cache, never published to the lake, and keyed by
//! a signature over the binding and the searched columns, so a projection captured
//! for other columns is never read.

use duckdb::Connection;
use sha2::{Digest, Sha256};

use super::sql_view::{is_managed_view, is_valid_identifier, quote_ident};
use crate::{Indexer, IndexerError};

/// Upper bound on the distinct search texts kept per view. Beyond it the
/// projection is partial (logged), so match counts become lower bounds.
pub const MAX_SEARCH_TEXTS: usize = 100_000;

const DDL: &str = "CREATE TABLE IF NOT EXISTS sql_view_search_text (\
     view VARCHAR NOT NULL, sig VARCHAR NOT NULL, text VARCHAR NOT NULL, n BIGINT NOT NULL);";

/// The skill's `search_text` columns that are safe identifiers (empty when the
/// skill is not a `sql_view` skill or declares none). Search and capture both
/// derive the columns here, so their signatures agree.
pub(crate) async fn search_columns(indexer: &Indexer, skill: &str) -> Vec<String> {
    let Ok(binding) = indexer.skill_backend(skill).await else {
        return Vec::new();
    };
    binding
        .sql_view
        .map(|sv| {
            sv.search_text
                .into_iter()
                .filter(|c| is_valid_identifier(c))
                .collect()
        })
        .unwrap_or_default()
}

/// The projection key: the overlay's binding hash plus the searched columns.
pub(crate) fn signature(binding_hash: &str, cols: &[String]) -> String {
    let mut h = Sha256::new();
    h.update(binding_hash.as_bytes());
    for c in cols {
        h.update(b"\0");
        h.update(c.as_bytes());
    }
    format!("{:x}", h.finalize())
}

/// Capture `view`'s search texts under `sig`, replacing whatever it held. On a
/// failure (source unreachable) the previous projection is kept.
pub(crate) async fn capture(
    indexer: &Indexer,
    view: &str,
    sig: &str,
    cols: &[String],
) -> Result<(), IndexerError> {
    if cols.is_empty() || !is_managed_view(view) {
        return Ok(());
    }
    let conn = indexer.conn.lock().await.try_clone()?;
    let texts = super::blocking_section(|| capture_on(&conn, view, sig, cols))?;
    if texts >= MAX_SEARCH_TEXTS {
        tracing::warn!(
            target: "escurel",
            view,
            cap = MAX_SEARCH_TEXTS,
            "sql_view search projection truncated: search match counts are lower bounds"
        );
    }
    Ok(())
}

fn capture_on(
    conn: &Connection,
    view: &str,
    sig: &str,
    cols: &[String],
) -> Result<usize, duckdb::Error> {
    conn.execute_batch(DDL)?;
    let text = cols
        .iter()
        .map(|c| format!("CAST({} AS VARCHAR)", quote_ident(c)))
        .collect::<Vec<_>>()
        .join(", ");
    // One row per distinct text, weighted by how many source rows carry it, so the
    // lane's score (rows matched) is unchanged. U+001F keeps columns apart.
    let insert = format!(
        "INSERT INTO sql_view_search_text \
         SELECT ?, ?, text, n FROM (\
           SELECT concat_ws(chr(31), {text}) AS text, count(*) AS n FROM {view} GROUP BY 1\
         ) WHERE text <> '' LIMIT {MAX_SEARCH_TEXTS}"
    );
    conn.execute_batch("BEGIN TRANSACTION")?;
    let r = conn
        .execute("DELETE FROM sql_view_search_text WHERE view = ?", [view])
        .and_then(|_| conn.execute(&insert, [view, sig]));
    match r {
        Ok(n) => conn.execute_batch("COMMIT").map(|()| n),
        Err(e) => {
            let _ = conn.execute_batch("ROLLBACK");
            Err(e)
        }
    }
}

/// Source rows of `view` whose search text contains `pattern` (an ILIKE
/// pattern), read from the local projection only. `0` when none was captured.
pub(crate) fn match_count(conn: &Connection, view: &str, sig: &str, pattern: &str) -> i64 {
    conn.query_row(
        "SELECT coalesce(sum(n), 0)::BIGINT FROM sql_view_search_text \
         WHERE view = ? AND sig = ? AND text ILIKE ?",
        [view, sig, pattern],
        |r| r.get(0),
    )
    .unwrap_or(0)
}
