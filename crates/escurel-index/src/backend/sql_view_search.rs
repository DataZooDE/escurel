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
    indexer
        .skill_backend(skill)
        .await
        .map(|b| columns_of(&b))
        .unwrap_or_default()
}

/// [`search_columns`] from a binding already in hand (a `list_skills` row).
pub(crate) fn columns_of(binding: &super::BackendBinding) -> Vec<String> {
    binding
        .sql_view
        .as_ref()
        .map(|sv| {
            sv.search_text
                .iter()
                .filter(|c| is_valid_identifier(c))
                .cloned()
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

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::{Duration, Instant};

    use escurel_embed::{Embedder, ZeroEmbedder};
    use escurel_storage::{FsStore, LaneStore};

    use crate::backend::{SqlConnector, SqlViewBackend, SqlViewBinding};
    use crate::{Indexer, Migrator};

    const SKILLS: usize = 120;
    const HOLD: Duration = Duration::from_millis(25);

    /// In the lab the async runner keeps the tenant's one connection busy
    /// (`list_events` holds it ~1.4 s per call, several times a minute). A lane
    /// that takes the connection once per skill queues behind that neighbour
    /// once per skill, so an unfiltered search over 56 skills waited for minutes
    /// while a single-skill one took seconds. Model the neighbour — hold the lock
    /// for `HOLD`, release, repeat — and require the lanes to take it a bounded
    /// number of times, independent of the number of skills: per-skill locking
    /// needs ≥ `SKILLS` waits (≥ 3 s here).
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn the_search_lanes_take_the_connection_a_bounded_number_of_times() {
        let store_dir = tempfile::TempDir::new().unwrap();
        let db_dir = tempfile::TempDir::new().unwrap();
        let data_dir = tempfile::TempDir::new().unwrap();
        let store: Arc<dyn LaneStore> = Arc::new(FsStore::new(store_dir.path().to_path_buf()));
        let embedder: Arc<dyn Embedder> = Arc::new(ZeroEmbedder::default());
        let conn = duckdb::Connection::open(db_dir.path().join("escurel.duckdb")).unwrap();
        Migrator::up(&conn).unwrap();
        let idx = Arc::new(Indexer::new(store, embedder, conn, "acme").unwrap());

        for i in 0..SKILLS {
            idx.update_page(
                &format!("markdown/skills/s{i}.md"),
                &format!("---\nkind: skill\nid: s{i}\ndescription: x\n---\n# s{i}\n"),
            )
            .await
            .unwrap();
        }
        idx.update_page(
            "markdown/skills/customers.md",
            "---\nkind: skill\nid: customers\ndescription: x\nbackend:\n  kind: sql_view\n  \
             source: { connector: json_dir, relation: /unused }\n  search_text: [title]\n---\n# c\n",
        )
        .await
        .unwrap();
        std::fs::write(data_dir.path().join("a.json"), br#"{"title":"widget"}"#).unwrap();
        let binding = SqlViewBinding {
            connector: SqlConnector::JsonDir,
            attach: None,
            relation: data_dir.path().to_str().unwrap().to_owned(),
            filter: None,
            project: Default::default(),
            search_text: vec!["title".to_owned()],
        };
        let m = SqlViewBackend::new(Arc::clone(&idx))
            .create_instance("customers", &binding, "eu", "# eu")
            .await
            .unwrap();

        let stop = Arc::new(AtomicBool::new(false));
        let neighbour = {
            let (idx, stop) = (Arc::clone(&idx), Arc::clone(&stop));
            tokio::spawn(async move {
                while !stop.load(Ordering::SeqCst) {
                    let guard = idx.conn.lock().await;
                    std::thread::sleep(HOLD);
                    drop(guard);
                    tokio::task::yield_now().await;
                }
            })
        };
        tokio::time::sleep(HOLD).await;

        let t = Instant::now();
        let sql = idx
            .sql_view_search_candidates("widget", None)
            .await
            .unwrap();
        let rows = idx.rows_search_candidates("widget", None).await.unwrap();
        let elapsed = t.elapsed();
        stop.store(true, Ordering::SeqCst);
        neighbour.await.unwrap();

        assert_eq!(
            sql.iter().map(|h| h.page_id.as_str()).collect::<Vec<_>>(),
            [m.page_id.as_str()]
        );
        assert!(rows.is_empty());
        assert!(
            elapsed < Duration::from_millis(800),
            "lanes took {elapsed:?} behind a busy connection ({SKILLS} skills, {HOLD:?} holds)"
        );
    }
}
