//! Run transcripts (#822) against a real DuckDB file and a real `FsStore`.
//! A transcript is KEY-addressed (`transcripts/<run_id>.jsonl`), written once
//! per run, and the retention sweep deletes transcript keys and nothing else
//! — in particular never a content-addressed document blob that happens to
//! hold the same bytes (crew review PR-1, F3/F15).

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use bytes::Bytes;
use duckdb::Connection;
use escurel_embed::{Embedder, ZeroEmbedder};
use escurel_index::{Indexer, IndexerError, Migrator};
use escurel_storage::{FsStore, Key, LaneStore};
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

#[tokio::test]
async fn a_document_with_a_transcripts_bytes_survives_replace_and_sweep() {
    let (store, indexer, _s, _d) = fresh();
    let bytes = Bytes::from_static(b"{\"kind\":\"message\"}\n");
    let document = store.put_blob(TENANT, bytes.clone(), None).await.unwrap();
    let stored = indexer
        .store_run_transcript("run-a", bytes.clone(), 1, CAP)
        .await
        .unwrap();
    assert_eq!((stored.records, stored.bytes), (1, 19));

    // A second, different upload for the same run is refused, and deletes nothing.
    let err = indexer
        .store_run_transcript("run-a", Bytes::from_static(b"{\"x\":1}\n"), 1, CAP)
        .await
        .expect_err("written once");
    assert!(
        matches!(err, IndexerError::TranscriptExists { .. }),
        "{err}"
    );
    assert!(store.get_blob(TENANT, &document).await.is_ok());

    // The sweep removes the transcript and never the document blob.
    let soon = SystemTime::now() + Duration::from_secs(1);
    assert_eq!(indexer.sweep_run_transcripts(soon).await.unwrap(), 1);
    assert!(
        indexer
            .read_run_transcript("run-a")
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        store.get_blob(TENANT, &document).await.unwrap(),
        bytes,
        "a document blob is never swept"
    );
}

#[tokio::test]
async fn a_transcript_is_written_once_and_an_identical_retry_is_a_no_op() {
    let (_store, indexer, _s, _d) = fresh();
    let bytes = Bytes::from_static(b"{\"kind\":\"message\"}\n");
    let first = indexer
        .store_run_transcript("run-a", bytes.clone(), 1, CAP)
        .await
        .unwrap();
    let again = indexer
        .store_run_transcript("run-a", bytes.clone(), 1, CAP)
        .await
        .unwrap();
    assert_eq!(first, again, "an identical retry is idempotent");
    assert_eq!(
        indexer.read_run_transcript("run-a").await.unwrap(),
        Some(bytes)
    );
    // Nothing is older than an hour ago.
    let hour_ago = SystemTime::now() - Duration::from_secs(3600);
    assert_eq!(indexer.sweep_run_transcripts(hour_ago).await.unwrap(), 0);
    assert!(
        indexer
            .read_run_transcript("run-a")
            .await
            .unwrap()
            .is_some()
    );
}

#[tokio::test]
async fn a_transcript_over_the_cap_or_with_an_unsafe_run_id_is_refused() {
    let (_store, indexer, _s, _d) = fresh();
    let err = indexer
        .store_run_transcript("run-a", Bytes::from(vec![b'x'; 2048]), 1, CAP)
        .await
        .expect_err("over the cap");
    assert!(err.to_string().contains("quota"), "{err}");
    for bad in ["../blobs/x", "a/b", "", ".."] {
        assert!(
            indexer
                .store_run_transcript(bad, Bytes::from_static(b"{}\n"), 1, CAP)
                .await
                .is_err(),
            "`{bad}` must not name a key"
        );
    }
}

/// The registry lives in DuckDB; the bytes in the lane store. After an index rebuild the rows are
/// gone but the keys are not: the sweep adopts an unregistered transcript key (as stored now), so
/// it expires one retention window later instead of living forever.
#[tokio::test]
async fn an_unregistered_transcript_key_is_adopted_by_the_sweep_and_then_expires() {
    let (store, indexer, _s, _d) = fresh();
    let key = Key::new(TENANT, "transcripts/run-orphan.jsonl").unwrap();
    store
        .write(&key, Bytes::from_static(b"{\"kind\":\"message\"}\n"))
        .await
        .unwrap();
    let soon = SystemTime::now() + Duration::from_secs(1);
    assert_eq!(indexer.sweep_run_transcripts(soon).await.unwrap(), 0);
    assert!(
        store.read(&key).await.is_ok(),
        "adopted, not deleted on sight"
    );
    tokio::time::sleep(Duration::from_millis(1100)).await;
    let later = SystemTime::now() + Duration::from_secs(1);
    assert_eq!(indexer.sweep_run_transcripts(later).await.unwrap(), 1);
    assert!(store.read(&key).await.is_err(), "expired once adopted");
}
