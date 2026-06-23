//! Pins FIX 6: `update_page` must NOT hold the DuckDB connection
//! mutex across the embedder `.await`. A slow (e.g. network) embed on
//! one page must leave the connection free for concurrent reads.
//!
//! The write-serialization invariant (two concurrent writers commit
//! in order, never out of order) is preserved by a dedicated
//! `write_lock` instead — that part is exercised by the existing
//! round-trip tests; here we prove the connection is genuinely free
//! during the embed.
//!
//! Real DuckDB + real FsStore (single connection); a test-only
//! `SlowEmbedder` is the only seam (it has to be controllable to make
//! the timing deterministic).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use duckdb::Connection;
use escurel_embed::{EmbedError, Embedder};
use escurel_index::{Indexer, Migrator};
use escurel_storage::{FsStore, Key, LaneStore};
use tempfile::TempDir;
use tokio::sync::Notify;

const TENANT: &str = "acme";
const DIM: usize = 768;

/// Embedder that is instant until `slow` is set; once slow, an embed
/// signals `started` then blocks on `release` — so the test can
/// observe the world *while* an embed is in flight.
struct SlowEmbedder {
    slow: AtomicBool,
    started: Arc<Notify>,
    release: Arc<Notify>,
}

#[async_trait]
impl Embedder for SlowEmbedder {
    fn dim(&self) -> usize {
        DIM
    }

    fn model_id(&self) -> String {
        "slow-test".to_owned()
    }

    async fn embed(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, EmbedError> {
        if self.slow.load(Ordering::SeqCst) {
            self.started.notify_one();
            self.release.notified().await;
        }
        Ok((0..texts.len()).map(|_| vec![0.0_f32; DIM]).collect())
    }
}

fn skill_md(id: &str) -> String {
    format!("---\ntype: skill\nid: {id}\ndescription: d\n---\n# {id}\n")
}

#[tokio::test]
async fn slow_embed_does_not_block_reads_on_the_connection() {
    let store_dir = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    let duckdb_path = db_dir.path().join("escurel.duckdb");

    let store: Arc<dyn LaneStore> = Arc::new(FsStore::new(store_dir.path().to_path_buf()));
    let started = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let embedder = Arc::new(SlowEmbedder {
        slow: AtomicBool::new(false),
        started: Arc::clone(&started),
        release: Arc::clone(&release),
    });
    let conn = Connection::open(&duckdb_path).unwrap();
    Migrator::up(&conn).unwrap();
    let indexer = Arc::new(
        Indexer::new(
            Arc::clone(&store),
            Arc::clone(&embedder) as Arc<dyn Embedder>,
            conn,
            TENANT,
        )
        .unwrap(),
    );

    // Seed a readable skill while the embedder is still instant.
    {
        let pid = "markdown/skills/existing.md";
        let body = skill_md("existing");
        store
            .write(&Key::new(TENANT, pid).unwrap(), Bytes::from(body.clone()))
            .await
            .unwrap();
        indexer.update_page(pid, &body).await.unwrap();
    }

    // Now make the embedder block, and kick off a write whose embed
    // parks mid-flight (NOT holding the connection mutex).
    embedder.slow.store(true, Ordering::SeqCst);
    let writer = {
        let indexer = Arc::clone(&indexer);
        let store = Arc::clone(&store);
        tokio::spawn(async move {
            let pid = "markdown/skills/slow.md";
            let body = skill_md("slow");
            store
                .write(&Key::new(TENANT, pid).unwrap(), Bytes::from(body.clone()))
                .await
                .unwrap();
            indexer.update_page(pid, &body).await
        })
    };

    // Wait until the embed is in flight (writer parked inside
    // SlowEmbedder::embed).
    started.notified().await;

    // While the embed is blocked, a read must complete promptly. If
    // `update_page` still held `conn` across the embed, this would
    // hang until the timeout.
    let read = tokio::time::timeout(Duration::from_secs(5), indexer.list_skills())
        .await
        .expect("read must NOT block on the connection during a slow embed")
        .expect("list_skills ok");
    assert!(
        read.iter().any(|s| s.id == "existing"),
        "the previously-indexed skill is visible during the slow embed: {read:?}"
    );

    // Let the embed finish; the write commits cleanly.
    release.notify_one();
    writer.await.unwrap().expect("update_page ok");

    // After commit, both skills are present.
    let all = indexer.list_skills().await.unwrap();
    assert_eq!(all.len(), 2, "{all:?}");
}
