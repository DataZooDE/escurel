//! The embedding model is part of the index's identity: an index reopened
//! under a different model re-embeds instead of mixing vector spaces, a
//! failed re-embed leaves the old vectors serving, a degraded boot keeps the
//! vectors it has, and rows written while degraded are repaired when the
//! model returns.
//!
//! Real DuckDB file + real file store, reopened across "restarts".

use std::sync::Arc;

use async_trait::async_trait;
use escurel_embed::{EmbedError, Embedder, ZeroEmbedder};
use escurel_index::embedding_space::EmbeddingSpaceOutcome;
use escurel_index::snapshot::{IndexStore, OpenedIndex, SingleFileStore};
use escurel_storage::{FsStore, LaneStore};
use tempfile::TempDir;

const TENANT: &str = "acme";
const PAGE: &str = "markdown/skills/note.md";
const LATER: &str = "markdown/skills/later.md";
const A: &str = "provider-a:model";
const B: &str = "provider-b:model";

/// Every vector is `value` in every component, under the identity `id`, so a
/// test can tell which model produced a row. `fail` makes every call fail, as
/// a provider out of quota does.
struct Constant {
    id: &'static str,
    value: f32,
    fail: bool,
}

fn model(id: &'static str, value: f32) -> Arc<dyn Embedder> {
    Arc::new(Constant {
        id,
        value,
        fail: false,
    })
}

#[async_trait]
impl Embedder for Constant {
    fn dim(&self) -> usize {
        768
    }
    fn model_id(&self) -> String {
        self.id.to_owned()
    }
    async fn embed(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, EmbedError> {
        if self.fail {
            return Err(EmbedError::Backend("HTTP 429: quota exhausted".to_owned()));
        }
        Ok(texts.iter().map(|_| vec![self.value; 768]).collect())
    }
}

struct Dirs {
    store: TempDir,
    tenant: TempDir,
}

async fn open(dirs: &Dirs, embedder: Arc<dyn Embedder>) -> OpenedIndex {
    let store: Arc<dyn LaneStore> = Arc::new(FsStore::new(dirs.store.path().to_path_buf()));
    SingleFileStore {
        tenant_dir: dirs.tenant.path().to_path_buf(),
        rebuild_on_boot: false,
        store,
        embedder,
        tenant: TENANT.to_owned(),
        contextualize: Default::default(),
        attach_retrieval: None,
        seed_dir: None,
    }
    .open()
    .await
    .expect("open")
}

/// The first component of the block vector of `page`.
fn vector(opened: &OpenedIndex, page: &str) -> Option<f32> {
    let conn = opened.crdt_conn.as_ref().expect("same-instance connection");
    conn.query_row(
        "SELECT dense_vec[1] FROM blocks WHERE page_id = ?",
        [page],
        |r| r.get::<_, Option<f32>>(0),
    )
    .expect("block")
}

/// Every block's first component, so "nothing mixed" covers the whole index.
fn all_vectors(opened: &OpenedIndex) -> Vec<Option<f32>> {
    let conn = opened.crdt_conn.as_ref().expect("connection");
    let mut stmt = conn
        .prepare("SELECT dense_vec[1] FROM blocks ORDER BY block_id")
        .expect("prepare");
    stmt.query_map([], |r| r.get::<_, Option<f32>>(0))
        .expect("query")
        .map(Result::unwrap)
        .collect()
}

async fn write(opened: &OpenedIndex, page: &str) {
    let id = page
        .trim_start_matches("markdown/skills/")
        .trim_end_matches(".md");
    opened
        .indexer
        .update_page(
            page,
            &format!("---\nkind: skill\nid: {id}\n---\n# {id}\n\nBody.\n"),
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn the_embedding_model_is_part_of_the_index_identity() {
    let dirs = Dirs {
        store: TempDir::new().unwrap(),
        tenant: TempDir::new().unwrap(),
    };

    // Boot 1, fresh, model A: the space is recorded at open, so nothing is
    // re-embedded (a container that rebuilds on every boot pays once).
    let opened = open(&dirs, model(A, 0.25)).await;
    assert_eq!(
        opened.indexer.reconcile_embedding_space().await.unwrap(),
        EmbeddingSpaceOutcome::Unchanged
    );
    write(&opened, PAGE).await;
    assert_eq!(vector(&opened, PAGE), Some(0.25));
    drop(opened);

    // Boot 2, model B, provider failing: the re-embed fails WITHOUT touching
    // the index — A's vectors and record keep serving.
    let failing: Arc<dyn Embedder> = Arc::new(Constant {
        id: B,
        value: 0.75,
        fail: true,
    });
    let opened = open(&dirs, failing).await;
    assert!(opened.indexer.reconcile_embedding_space().await.is_err());
    assert!(all_vectors(&opened).iter().all(|v| *v == Some(0.25)));
    assert_eq!(
        opened.indexer.recorded_embedding_space().await.unwrap(),
        Some((A.to_owned(), 768))
    );
    drop(opened);

    // Boot 3, model B: every block is re-embedded, none left in A's space.
    let opened = open(&dirs, model(B, 0.75)).await;
    let outcome = opened.indexer.reconcile_embedding_space().await.unwrap();
    assert!(
        matches!(
            &outcome,
            EmbeddingSpaceOutcome::Reembedded { from: Some(from), blocks, chat_vectors_cleared: true }
                if from == A && *blocks >= 1
        ),
        "{outcome:?}"
    );
    assert!(all_vectors(&opened).iter().all(|v| *v == Some(0.75)));
    drop(opened);

    // Boot 4, degraded: B's vectors and record survive; a write lands a zero
    // vector, which is all a degraded embedder has.
    let opened = open(&dirs, Arc::new(ZeroEmbedder::default())).await;
    assert_eq!(
        opened.indexer.reconcile_embedding_space().await.unwrap(),
        EmbeddingSpaceOutcome::KeptWhileDegraded {
            recorded: B.to_owned()
        }
    );
    assert_eq!(vector(&opened, PAGE), Some(0.75));
    write(&opened, LATER).await;
    assert_eq!(vector(&opened, LATER), Some(0.0));
    drop(opened);

    // Boot 5, B again: exactly the degraded write is repaired.
    let opened = open(&dirs, model(B, 0.75)).await;
    assert_eq!(
        opened.indexer.reconcile_embedding_space().await.unwrap(),
        EmbeddingSpaceOutcome::Repaired { blocks: 1 }
    );
    assert_eq!(vector(&opened, LATER), Some(0.75));
    assert!(all_vectors(&opened).iter().all(|v| *v == Some(0.75)));

    // An index from before the record existed: vectors of unknown origin are
    // re-embedded, never assumed to be the live model's.
    opened
        .crdt_conn
        .as_ref()
        .unwrap()
        .execute("DELETE FROM embedding_space", [])
        .unwrap();
    drop(opened);
    let opened = open(&dirs, model(A, 0.25)).await;
    assert!(matches!(
        opened.indexer.reconcile_embedding_space().await.unwrap(),
        EmbeddingSpaceOutcome::Reembedded { from: None, .. }
    ));
    assert!(all_vectors(&opened).iter().all(|v| *v == Some(0.25)));

    // A dimension the columns cannot hold is refused before anything runs.
    struct Wide;
    #[async_trait]
    impl Embedder for Wide {
        fn dim(&self) -> usize {
            1024
        }
        fn model_id(&self) -> String {
            "wide".to_owned()
        }
        async fn embed(&self, _: &[&str]) -> Result<Vec<Vec<f32>>, EmbedError> {
            unreachable!("refused before embedding")
        }
    }
    assert!(
        opened
            .indexer
            .reconcile_embedding_space_with(&Wide, || panic!("not activated"))
            .await
            .is_err()
    );
}
