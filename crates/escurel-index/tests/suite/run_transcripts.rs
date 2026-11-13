//! Run transcripts (#822) against a real DuckDB file and a real `FsStore`:
//! the retention sweep removes a transcript's row and blob, keeps a blob
//! another run's transcript still names, and never touches a blob that is
//! not a transcript.

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use bytes::Bytes;
use duckdb::Connection;
use escurel_embed::{Embedder, ZeroEmbedder};
use escurel_index::{Indexer, Migrator};
use escurel_storage::{BlobId, FsStore, LaneStore};
use tempfile::TempDir;

const TENANT: &str = "acme";
const CAP: u64 = 1024;

fn fresh() -> (Arc<dyn LaneStore>, Indexer, TempDir, TempDir) {
    let store_dir = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    let store: Arc<dyn LaneStore> = Arc::new(FsStore::new(store_dir.path().to_path_buf()));
    let embedder: Arc<dyn Embedder> = Arc::new(ZeroEmbedder::default());
    let conn = Connection::open(db_dir.path().join("escurel.duckdb")).unwrap();
    Migrator::up(&conn).unwrap();
    let indexer = Indexer::new(Arc::clone(&store), embedder, conn, TENANT).unwrap();
    (store, indexer, store_dir, db_dir)
}

async fn exists(store: &Arc<dyn LaneStore>, blob_id: &str) -> bool {
    store
        .get_blob(TENANT, &BlobId::parse(blob_id).unwrap())
        .await
        .is_ok()
}

#[tokio::test]
async fn the_sweep_deletes_expired_transcripts_and_only_unshared_transcript_blobs() {
    let (store, indexer, _s, _d) = fresh();
    let document = store
        .put_blob(
            TENANT,
            Bytes::from_static(b"a document, not a transcript"),
            None,
        )
        .await
        .unwrap();
    let same = Bytes::from_static(b"{\"kind\":\"message\"}\n");
    let a = indexer
        .store_run_transcript("run-a", same.clone(), 1, CAP)
        .await
        .unwrap();
    let b = indexer
        .store_run_transcript("run-b", same, 1, CAP)
        .await
        .unwrap();
    assert_eq!(a.blob_id, b.blob_id, "content-addressed");
    assert_eq!((a.records, a.bytes), (1, 19));

    // Idempotent: the same upload again changes nothing.
    let again = indexer
        .store_run_transcript(
            "run-a",
            Bytes::from_static(b"{\"kind\":\"message\"}\n"),
            1,
            CAP,
        )
        .await
        .unwrap();
    assert_eq!(again, a);

    // Nothing is older than an hour ago.
    let hour_ago = SystemTime::now() - Duration::from_secs(3600);
    assert_eq!(indexer.sweep_run_transcripts(hour_ago).await.unwrap(), 0);
    assert!(exists(&store, &a.blob_id).await);

    // Everything is older than a second from now: both rows go, the shared
    // blob goes once nobody names it, the document blob stays.
    let soon = SystemTime::now() + Duration::from_secs(1);
    assert_eq!(indexer.sweep_run_transcripts(soon).await.unwrap(), 2);
    assert!(
        !exists(&store, &a.blob_id).await,
        "the transcript blob is gone"
    );
    assert!(
        exists(&store, document.as_str()).await,
        "a document blob is never swept"
    );
}

#[tokio::test]
async fn a_transcript_over_the_cap_is_refused() {
    let (_store, indexer, _s, _d) = fresh();
    let err = indexer
        .store_run_transcript("run-a", Bytes::from(vec![b'x'; 2048]), 1, CAP)
        .await
        .expect_err("over the cap");
    assert!(err.to_string().contains("quota"), "{err}");
}

#[tokio::test]
async fn a_replaced_transcript_drops_its_old_blob() {
    let (store, indexer, _s, _d) = fresh();
    let first = indexer
        .store_run_transcript("run-a", Bytes::from_static(b"one\n"), 1, CAP)
        .await
        .unwrap();
    let second = indexer
        .store_run_transcript("run-a", Bytes::from_static(b"one\ntwo\n"), 2, CAP)
        .await
        .unwrap();
    assert_ne!(first.blob_id, second.blob_id);
    assert!(!exists(&store, &first.blob_id).await);
    assert!(exists(&store, &second.blob_id).await);
}
