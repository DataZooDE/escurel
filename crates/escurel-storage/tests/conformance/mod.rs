//! The backend-agnostic `LaneStore` contract, in one place.
//!
//! Every `LaneStore` implementation must pass [`run_lane_store_conformance`].
//! Before this existed, `fs_roundtrip.rs` and `s3_roundtrip.rs` hand-mirrored
//! the same ~11 scenarios and drifted: the S3 side was missing
//! `delete_missing_returns_not_found` and the distinct-versions check, neither
//! backend covered `size()` at all, and the blob layer (`blobs.rs`) only ever
//! ran against `FsStore` — even though its default trait methods ride on
//! `read`/`write`/`list`, so a backend with a subtly different `list()`
//! contract would break blobs silently.
//!
//! Backend-SPECIFIC behaviour stays in the per-backend file: filesystem parent
//! -directory creation and `.tmp` orphan cleanup, the exact URL scheme, and
//! anything about client construction.
//!
//! `scope` namespaces every key this suite touches, so a backend backed by a
//! real, reused bucket can run it repeatedly without cross-run collisions.
//! Callers pass a short unique string.

use bytes::Bytes;
use escurel_storage::{BlobId, Key, LaneStore, StoreError};

fn k(tenant: &str, path: &str) -> Key {
    Key::new(tenant, path).expect("valid key")
}

/// Run the full contract. Panics with a scenario-named message on failure.
pub async fn run_lane_store_conformance(store: &dyn LaneStore, scope: &str) {
    write_then_read_roundtrip(store, scope).await;
    overwrite_replaces_bytes(store, scope).await;
    read_missing_returns_not_found(store, scope).await;
    delete_then_read_returns_not_found(store, scope).await;
    delete_missing_returns_not_found(store, scope).await;
    list_returns_keys_under_prefix(store, scope).await;
    list_under_nonexistent_prefix_returns_empty(store, scope).await;
    list_isolates_tenants(store, scope).await;
    write_returns_distinct_versions(store, scope).await;
    size_matches_written_bytes(store, scope).await;
    size_of_missing_returns_not_found(store, scope).await;
    url_is_parseable_and_carries_the_path(store, scope).await;
    blob_put_is_content_addressed_and_idempotent(store, scope).await;
    blob_size_quota_is_enforced_before_write(store, scope).await;
    blob_inbox_deposit_then_promote(store, scope).await;
    blob_list_excludes_the_inbox(store, scope).await;
}

async fn write_then_read_roundtrip(store: &dyn LaneStore, scope: &str) {
    let key = k(&format!("{scope}-rt"), "markdown/skills/customer.md");
    let body = Bytes::from_static(b"---\ntitle: Customer\n---\n\nBody.\n");
    let version = store.write(&key, body.clone()).await.expect("write");
    assert!(
        !version.is_empty(),
        "write must return a non-empty version identifier",
    );
    assert_eq!(store.read(&key).await.expect("read"), body);
}

async fn overwrite_replaces_bytes(store: &dyn LaneStore, scope: &str) {
    let key = k(&format!("{scope}-ow"), "markdown/skills/customer.md");
    store
        .write(&key, Bytes::from_static(b"first"))
        .await
        .expect("write 1");
    store
        .write(&key, Bytes::from_static(b"second"))
        .await
        .expect("write 2");
    assert_eq!(
        store.read(&key).await.expect("read"),
        Bytes::from_static(b"second"),
        "an overwrite must fully replace the body, not append or merge",
    );
}

async fn read_missing_returns_not_found(store: &dyn LaneStore, scope: &str) {
    let key = k(&format!("{scope}-miss"), "markdown/skills/nope.md");
    assert!(
        matches!(store.read(&key).await, Err(StoreError::NotFound(_))),
        "reading an absent key must be NotFound, not an empty body",
    );
}

async fn delete_then_read_returns_not_found(store: &dyn LaneStore, scope: &str) {
    let key = k(&format!("{scope}-del"), "markdown/skills/gone.md");
    store
        .write(&key, Bytes::from_static(b"x"))
        .await
        .expect("write");
    store.delete(&key).await.expect("delete");
    assert!(matches!(
        store.read(&key).await,
        Err(StoreError::NotFound(_))
    ));
}

async fn delete_missing_returns_not_found(store: &dyn LaneStore, scope: &str) {
    // S3 DeleteObject is idempotent (204 on a missing key), so the S3 backend
    // HEADs first to honour this. That deliberate extra round-trip is exactly
    // the kind of thing a shared suite exists to keep honest.
    let key = k(&format!("{scope}-delmiss"), "markdown/skills/never.md");
    assert!(
        matches!(store.delete(&key).await, Err(StoreError::NotFound(_))),
        "deleting an absent key must be NotFound, not a silent success",
    );
}

async fn list_returns_keys_under_prefix(store: &dyn LaneStore, scope: &str) {
    let tenant = format!("{scope}-list");
    for p in [
        "markdown/skills/a.md",
        "markdown/skills/b.md",
        "markdown/instances/customer/acme.md",
    ] {
        store
            .write(&k(&tenant, p), Bytes::from_static(b"x"))
            .await
            .expect("write");
    }
    let mut got: Vec<String> = store
        .list(&k(&tenant, "markdown/skills"))
        .await
        .expect("list")
        .iter()
        .map(|key| key.path().to_owned())
        .collect();
    got.sort();
    assert_eq!(
        got,
        vec!["markdown/skills/a.md", "markdown/skills/b.md"],
        "list must return tenant-relative paths under the prefix only",
    );
}

async fn list_under_nonexistent_prefix_returns_empty(store: &dyn LaneStore, scope: &str) {
    let got = store
        .list(&k(&format!("{scope}-emptylist"), "markdown/nothing"))
        .await
        .expect("list must succeed on an absent prefix, not error");
    assert!(got.is_empty());
}

async fn list_isolates_tenants(store: &dyn LaneStore, scope: &str) {
    let (a, b) = (format!("{scope}-ta"), format!("{scope}-tb"));
    store
        .write(&k(&a, "markdown/skills/a.md"), Bytes::from_static(b"a"))
        .await
        .expect("write a");
    store
        .write(&k(&b, "markdown/skills/b.md"), Bytes::from_static(b"b"))
        .await
        .expect("write b");

    let got = store.list(&k(&a, "")).await.expect("list a");
    assert_eq!(
        got.len(),
        1,
        "a tenant listing must not leak another's keys"
    );
    assert_eq!(got[0].tenant(), a);
}

async fn write_returns_distinct_versions(store: &dyn LaneStore, scope: &str) {
    let key = k(&format!("{scope}-ver"), "markdown/skills/versioned.md");
    let v1 = store
        .write(&key, Bytes::from_static(b"one"))
        .await
        .expect("write 1");
    let v2 = store
        .write(&key, Bytes::from_static(b"two"))
        .await
        .expect("write 2");
    assert_ne!(
        v1, v2,
        "two writes of different bytes must yield different versions — \
         callers use this to detect concurrent modification",
    );
}

async fn size_matches_written_bytes(store: &dyn LaneStore, scope: &str) {
    let key = k(&format!("{scope}-size"), "markdown/skills/sized.md");
    let body = Bytes::from_static(b"0123456789");
    store.write(&key, body.clone()).await.expect("write");
    assert_eq!(
        store.size(&key).await.expect("size"),
        body.len() as u64,
        "size() must agree with the written body length",
    );
}

async fn size_of_missing_returns_not_found(store: &dyn LaneStore, scope: &str) {
    let key = k(&format!("{scope}-sizemiss"), "markdown/skills/absent.md");
    assert!(
        matches!(store.size(&key).await, Err(StoreError::NotFound(_))),
        "size() of an absent key must be NotFound, not 0",
    );
}

async fn url_is_parseable_and_carries_the_path(store: &dyn LaneStore, scope: &str) {
    // Scheme is backend-specific (file:// vs s3:// vs gs://) and asserted in
    // the per-backend file; what every backend owes is a parseable URL whose
    // path still identifies the object.
    let key = k(&format!("{scope}-url"), "markdown/skills/customer.md");
    store
        .write(&key, Bytes::from_static(b"x"))
        .await
        .expect("write");
    let url = store.url(&key).expect("url");
    assert!(
        url.as_str().contains("markdown/skills/customer.md"),
        "url must carry the object path, got: {url}",
    );
}

// --- blob layer -------------------------------------------------------
//
// These are default trait methods riding on read/write/list, so they are
// exactly where a backend's `list()` contract drift shows up.

async fn blob_put_is_content_addressed_and_idempotent(store: &dyn LaneStore, scope: &str) {
    let tenant = format!("{scope}-blob");
    let body = Bytes::from_static(b"%PDF-1.7 fake");
    let id1 = store
        .put_blob(&tenant, body.clone(), None)
        .await
        .expect("put 1");
    let id2 = store
        .put_blob(&tenant, body.clone(), None)
        .await
        .expect("put 2");
    assert_eq!(id1, id2, "the same bytes must map to the same blob id");
    assert_eq!(id1, BlobId::of(&body), "blob id must be sha256 of the body");
    assert_eq!(store.get_blob(&tenant, &id1).await.expect("get"), body);
}

async fn blob_size_quota_is_enforced_before_write(store: &dyn LaneStore, scope: &str) {
    let tenant = format!("{scope}-quota");
    let body = Bytes::from_static(b"0123456789");
    let err = store.put_blob(&tenant, body.clone(), Some(4)).await;
    assert!(
        matches!(
            err,
            Err(StoreError::BlobTooLarge {
                limit: 4,
                actual: 10
            })
        ),
        "an oversize blob must be rejected before any write",
    );
    // And nothing was stored.
    assert!(
        matches!(
            store.get_blob(&tenant, &BlobId::of(&body)).await,
            Err(StoreError::NotFound(_)),
        ),
        "a rejected blob must not have been written",
    );
}

async fn blob_inbox_deposit_then_promote(store: &dyn LaneStore, scope: &str) {
    let tenant = format!("{scope}-inbox");
    let body = Bytes::from_static(b"uploaded original");
    let id = store
        .put_inbox_blob(&tenant, body.clone(), None)
        .await
        .expect("deposit");
    assert_eq!(
        store.get_inbox_blob(&tenant, &id).await.expect("get inbox"),
        body
    );

    store
        .promote_inbox_blob(&tenant, &id)
        .await
        .expect("promote");
    assert_eq!(
        store.get_blob(&tenant, &id).await.expect("get canonical"),
        body,
        "promotion must move the bytes to the canonical area intact",
    );
    assert!(
        matches!(
            store.get_inbox_blob(&tenant, &id).await,
            Err(StoreError::NotFound(_)),
        ),
        "promotion must clear the inbox copy",
    );
}

async fn blob_list_excludes_the_inbox(store: &dyn LaneStore, scope: &str) {
    let tenant = format!("{scope}-bloblist");
    let canonical = store
        .put_blob(&tenant, Bytes::from_static(b"canonical"), None)
        .await
        .expect("put canonical");
    store
        .put_inbox_blob(&tenant, Bytes::from_static(b"staged"), None)
        .await
        .expect("deposit");

    let listed = store.list_blobs(&tenant).await.expect("list_blobs");
    assert_eq!(
        listed,
        vec![canonical],
        "list_blobs must return canonical blobs only, never inbox entries",
    );
}
