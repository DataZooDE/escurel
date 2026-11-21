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

/// What one [`crate::Indexer::refresh_search_projections`] pass did, per view.
/// A failed view keeps its previous projection.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ProjectionRefresh {
    /// Views whose projection was (re)captured, with the distinct texts kept.
    pub captured: Vec<(String, usize)>,
    /// Views whose capture failed, with the error.
    pub failed: Vec<(String, String)>,
}

/// The skill's `search_text` columns that are safe identifiers (empty when the
/// skill is not a `sql_view` skill or declares none), read from the skill page.
/// The search lane derives the same columns from a `list_skills` row with
/// [`columns_of`]; both go through it, so the signatures agree.
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

/// Capture `view`'s search texts under `sig`, replacing whatever it held, and
/// return how many distinct texts were kept. On a failure (source unreachable)
/// the previous projection is kept.
pub(crate) async fn capture(
    indexer: &Indexer,
    view: &str,
    sig: &str,
    cols: &[String],
) -> Result<usize, IndexerError> {
    if cols.is_empty() || !is_managed_view(view) {
        tracing::debug!(target: "escurel", view, "sql_view search projection skipped: no searchable columns");
        return Ok(0);
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
    Ok(texts)
}

fn capture_on(
    conn: &Connection,
    view: &str,
    sig: &str,
    cols: &[String],
) -> Result<usize, duckdb::Error> {
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
/// pattern), read from the local projection only. `0` when none was captured;
/// an error is the caller's to report (it must not read as "no match").
pub(crate) fn match_count(
    conn: &Connection,
    view: &str,
    sig: &str,
    pattern: &str,
) -> Result<i64, duckdb::Error> {
    conn.query_row(
        "SELECT coalesce(sum(n), 0)::BIGINT FROM sql_view_search_text \
         WHERE view = ? AND sig = ? AND text ILIKE ?",
        [view, sig, pattern],
        |r| r.get(0),
    )
}

#[cfg(test)]
mod tests {
    //! In-crate because the neighbour below must hold `Indexer::conn` itself
    //! (crate-private): that is the contention these tests are about.
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::{Duration, Instant};

    use escurel_embed::{Embedder, ZeroEmbedder};
    use escurel_storage::{FsStore, LaneStore};
    use serde_json::json;

    use crate::backend::{SqlConnector, SqlViewBackend, SqlViewBinding};
    use crate::{AclCaller, Indexer, Migrator};

    const SKILLS: usize = 120;
    const INSTANCES: usize = 12;
    const HITS: usize = 120;
    const HOLD: Duration = Duration::from_millis(25);
    /// Well under one wait per skill / instance / hit (each ≥ `HOLD`), well over
    /// a handful of waits plus the work.
    const BOUND: Duration = Duration::from_millis(800);

    struct Fixture {
        idx: Arc<Indexer>,
        _dirs: Vec<tempfile::TempDir>,
    }

    fn fixture() -> Fixture {
        let store_dir = tempfile::TempDir::new().unwrap();
        let db_dir = tempfile::TempDir::new().unwrap();
        let store: Arc<dyn LaneStore> = Arc::new(FsStore::new(store_dir.path().to_path_buf()));
        let embedder: Arc<dyn Embedder> = Arc::new(ZeroEmbedder::default());
        let conn = duckdb::Connection::open(db_dir.path().join("escurel.duckdb")).unwrap();
        Migrator::up(&conn).unwrap();
        let idx = Arc::new(Indexer::new(store, embedder, conn, "acme").unwrap());
        Fixture {
            idx,
            _dirs: vec![store_dir, db_dir],
        }
    }

    /// The lab's async runner, modelled: take the connection, hold it for
    /// `HOLD`, release, repeat. Returns once the neighbour has held it once, so
    /// the measured section always starts against a busy connection.
    async fn busy_neighbour(idx: &Arc<Indexer>) -> (Arc<AtomicBool>, tokio::task::JoinHandle<()>) {
        let stop = Arc::new(AtomicBool::new(false));
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let handle = {
            let (idx, stop) = (Arc::clone(idx), Arc::clone(&stop));
            tokio::spawn(async move {
                let mut started = Some(started_tx);
                while !stop.load(Ordering::SeqCst) {
                    let guard = idx.conn.lock().await;
                    if let Some(tx) = started.take() {
                        let _ = tx.send(());
                    }
                    std::thread::sleep(HOLD);
                    drop(guard);
                    tokio::task::yield_now().await;
                }
            })
        };
        started_rx.await.unwrap();
        (stop, handle)
    }

    /// In the lab the async runner keeps the tenant's one connection busy
    /// (`list_events` holds it ~1.4 s per call). A lane that takes the
    /// connection once per skill or per instance queues behind that neighbour
    /// each time, so an unfiltered search over 56 skills waited for minutes. The
    /// lanes must take it a bounded number of times, independent of the number
    /// of skills and instances (per-skill locking needs ≥ `SKILLS` waits, ≥ 3 s).
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn the_search_lanes_take_the_connection_a_bounded_number_of_times() {
        let f = fixture();
        let idx = &f.idx;
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
        let data = tempfile::TempDir::new().unwrap();
        std::fs::write(data.path().join("a.json"), br#"{"title":"widget"}"#).unwrap();
        let binding = SqlViewBinding {
            connector: SqlConnector::JsonDir,
            attach: None,
            relation: data.path().to_str().unwrap().to_owned(),
            filter: None,
            project: Default::default(),
            search_text: vec!["title".to_owned()],
        };
        let backend = SqlViewBackend::new(Arc::clone(idx));
        let mut pages = Vec::new();
        for i in 0..INSTANCES {
            let m = backend
                .create_instance("customers", &binding, &format!("eu{i}"), "# eu")
                .await
                .unwrap();
            pages.push(m.page_id);
        }
        let skills = idx.list_skills().await.unwrap();

        let (stop, neighbour) = busy_neighbour(idx).await;
        let t = Instant::now();
        let sql = idx
            .sql_view_search_candidates_with("widget", None, &skills)
            .await
            .unwrap();
        let rows = idx
            .rows_search_candidates_with("widget", None, &skills)
            .await
            .unwrap();
        let elapsed = t.elapsed();
        stop.store(true, Ordering::SeqCst);
        neighbour.await.unwrap();

        let mut found: Vec<String> = sql.into_iter().map(|h| h.page_id).collect();
        found.sort();
        pages.sort();
        assert_eq!(found, pages);
        assert!(rows.is_empty());
        assert!(
            elapsed < BOUND,
            "lanes took {elapsed:?} behind a busy connection \
             ({SKILLS} skills, {INSTANCES} instances, {HOLD:?} holds)"
        );
    }

    /// The same contention on the ACL filter: for a non-admin caller every hit
    /// used to cost three or more acquisitions (the skill's ACL, the tenant
    /// defaults, the caller's groups), per lane, per query variant. Decided
    /// against one `AclContext`, a batch of hits takes a bounded number.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn deciding_many_hits_for_a_non_admin_takes_the_connection_a_bounded_number_of_times() {
        let f = fixture();
        let idx = &f.idx;
        idx.update_page(
            "markdown/skills/note.md",
            "---\nkind: skill\nid: note\ndescription: x\nacl:\n  read: [team]\n---\n# note\n",
        )
        .await
        .unwrap();
        let team = vec!["team".to_owned()];
        let member = AclCaller {
            subject: "u1",
            token_groups: &team,
            ..AclCaller::default()
        };
        let outsider = AclCaller {
            subject: "u2",
            ..AclCaller::default()
        };
        let fm = json!({"id": "n1", "skill": "note"});
        let skills = idx.list_skills().await.unwrap();

        let (stop, neighbour) = busy_neighbour(idx).await;
        let t = Instant::now();
        let ctx = idx.acl_context(&member, &skills).await.unwrap();
        let out = idx.acl_context(&outsider, &skills).await.unwrap();
        for _ in 0..HITS {
            assert!(
                idx.may_read_instance_in(&ctx, &member, "note", &fm)
                    .await
                    .unwrap()
            );
            assert!(
                !idx.may_read_instance_in(&out, &outsider, "note", &fm)
                    .await
                    .unwrap()
            );
        }
        let elapsed = t.elapsed();
        stop.store(true, Ordering::SeqCst);
        neighbour.await.unwrap();

        // Same answers as the one-hit path.
        assert!(idx.may_read_instance(&member, "note", &fm).await.unwrap());
        assert!(!idx.may_read_instance(&outsider, "note", &fm).await.unwrap());
        assert!(
            elapsed < BOUND,
            "{} decisions took {elapsed:?} behind a busy connection ({HOLD:?} holds)",
            2 * HITS
        );
    }
}
