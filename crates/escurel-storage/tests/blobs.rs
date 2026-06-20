//! Integration tests for content-addressed blob storage (PR-3a).
//! Real `FsStore` over a tempdir. No mocks.

use std::sync::Arc;

use bytes::Bytes;
use escurel_storage::{BlobId, FsStore, LaneStore};
use tempfile::TempDir;

const TENANT: &str = "acme";

fn store() -> (Arc<dyn LaneStore>, TempDir) {
    let dir = TempDir::new().unwrap();
    let s: Arc<dyn LaneStore> = Arc::new(FsStore::new(dir.path().to_path_buf()));
    (s, dir)
}

#[tokio::test]
async fn put_blob_is_content_addressed_and_idempotent() {
    let (s, _d) = store();
    let body = Bytes::from_static(b"a born-digital PDF's bytes");

    let id1 = s.put_blob(TENANT, body.clone(), None).await.unwrap();
    let id2 = s.put_blob(TENANT, body.clone(), None).await.unwrap();
    assert_eq!(id1, id2, "same bytes → same id (content-addressed)");
    assert_eq!(id1, BlobId::of(&body), "id is the sha256 of the content");
    assert!(id1.as_str().starts_with("sha256:"));

    // Round-trips byte-for-byte.
    let got = s.get_blob(TENANT, &id1).await.unwrap();
    assert_eq!(got, body);

    // Different bytes → different id.
    let other = s
        .put_blob(TENANT, Bytes::from_static(b"different"), None)
        .await
        .unwrap();
    assert_ne!(other, id1);

    // Listed exactly once despite the duplicate put.
    let mut listed = s.list_blobs(TENANT).await.unwrap();
    listed.sort_by(|a, b| a.as_str().cmp(b.as_str()));
    assert_eq!(listed.len(), 2, "got {listed:?}");
    assert!(listed.contains(&id1) && listed.contains(&other));
}

#[tokio::test]
async fn blob_size_quota_enforced_before_write() {
    let (s, _d) = store();
    let big = Bytes::from(vec![0u8; 2048]);
    let err = s.put_blob(TENANT, big, Some(1024)).await.unwrap_err();
    assert!(
        matches!(
            err,
            escurel_storage::StoreError::BlobTooLarge {
                limit: 1024,
                actual: 2048
            }
        ),
        "got {err:?}"
    );
    // Nothing was written.
    assert!(s.list_blobs(TENANT).await.unwrap().is_empty());
}

#[tokio::test]
async fn inbox_deposit_then_promote_to_canonical() {
    let (s, _d) = store();
    let body = Bytes::from_static(b"scanned report");

    // Deposit into the inbox first (the canonical original lands before
    // processing; an upload is never lost).
    let id = s.put_inbox_blob(TENANT, body.clone(), None).await.unwrap();
    assert_eq!(s.get_inbox_blob(TENANT, &id).await.unwrap(), body);
    // Not yet in the canonical area.
    assert!(s.list_blobs(TENANT).await.unwrap().is_empty());

    // Promote after a successful materialise.
    s.promote_inbox_blob(TENANT, &id).await.unwrap();
    assert_eq!(s.get_blob(TENANT, &id).await.unwrap(), body);
    assert_eq!(s.list_blobs(TENANT).await.unwrap(), vec![id.clone()]);
    // The inbox copy is gone.
    assert!(s.get_inbox_blob(TENANT, &id).await.is_err());
}

#[tokio::test]
async fn blob_persists_across_store_reopen() {
    // Blobs are part of the canonical corpus — a fresh store over the same
    // root sees them (the property tenant_export relies on).
    let dir = TempDir::new().unwrap();
    let root = dir.path().to_path_buf();
    let body = Bytes::from_static(b"retained original");
    let id = {
        let s: Arc<dyn LaneStore> = Arc::new(FsStore::new(root.clone()));
        s.put_blob(TENANT, body.clone(), None).await.unwrap()
    };
    let s2: Arc<dyn LaneStore> = Arc::new(FsStore::new(root));
    assert_eq!(s2.get_blob(TENANT, &id).await.unwrap(), body);
}

#[test]
fn blob_id_parse_validates_shape() {
    assert!(BlobId::parse("sha256:not-hex").is_none());
    assert!(BlobId::parse("deadbeef").is_none());
    let valid = format!("sha256:{}", "a".repeat(64));
    assert_eq!(BlobId::parse(&valid).unwrap().as_str(), valid);
}
