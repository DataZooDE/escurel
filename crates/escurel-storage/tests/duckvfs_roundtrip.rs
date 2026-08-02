//! Integration tests for [`escurel_storage::DuckVfsStore`].
//!
//! Run against a `file://` root over a real `tempfile::TempDir` — no mocks,
//! no network, no credentials. That is deliberate and is the main reason
//! this backend is worth testing at all locally: the DuckDB VFS dispatches
//! on the path's scheme, so the code exercised here is byte-for-byte the
//! code that runs against `gdrive://`. Only the filesystem underneath
//! changes. A bug in key mapping, listing, or the not-found classification
//! shows up here, cheaply, instead of against live Drive.
//!
//! What this canNOT cover is Drive-specific behaviour: shared-drive
//! scoping, export-on-read, upload-on-close atomicity, eventual consistency
//! in listings. Those need the live suite.
//!
//! Requires `ESCUREL_TEST_GDRIVE_EXTENSION` to point at a built
//! `gdrive.duckdb_extension`, because `write_blob`/`remove_file`/`file_size`
//! live there. Without it the tests skip rather than fail — a checkout that
//! has not built the sibling extension should not have a red suite.

// Same gate as `s3_roundtrip.rs`: an integration test is its own crate and is
// compiled by a plain `cargo test --workspace`, so without this the
// feature-gated `DuckVfsStore` import fails to resolve on a default build.
#![cfg(feature = "duckvfs")]

use bytes::Bytes;
use escurel_storage::{DuckVfsStore, DuckVfsStoreConfig, Key, LaneStore, StoreError};
use tempfile::TempDir;

mod conformance;

/// `None` when the extension is not available, so every test can skip.
fn store_and_dir() -> Option<(DuckVfsStore, TempDir)> {
    let extension_path = std::env::var("ESCUREL_TEST_GDRIVE_EXTENSION").ok()?;
    let dir = TempDir::new().expect("tempdir");
    let root = format!("file://{}", dir.path().display());
    let store = DuckVfsStore::new(&DuckVfsStoreConfig {
        root,
        extension_path: Some(extension_path),
        drive_id: None,
        drive_scope: None,
    })
    .expect("open duckvfs store");
    Some((store, dir))
}

/// Emitted once per skipped test so a green run that tested nothing is not
/// mistaken for a green run that tested everything.
fn skip(name: &str) {
    eprintln!("SKIP {name}: set ESCUREL_TEST_GDRIVE_EXTENSION to a built gdrive.duckdb_extension");
}

fn k(tenant: &str, path: &str) -> Key {
    Key::new(tenant.to_owned(), path.to_owned())
        .unwrap_or_else(|err| panic!("test fixture key ({tenant:?}, {path:?}): {err}"))
}

/// The whole shared LaneStore contract, the same suite FsStore and S3Store
/// are held to. This is the point of the file: a new backend either passes
/// the existing conformance suite or it is not a LaneStore.
#[tokio::test]
async fn passes_lane_store_conformance() {
    let Some((store, _dir)) = store_and_dir() else {
        return skip("passes_lane_store_conformance");
    };
    conformance::run_lane_store_conformance(&store, "duckvfs").await;
}

#[tokio::test]
async fn write_then_read_roundtrip_is_byte_exact() {
    let Some((store, _dir)) = store_and_dir() else {
        return skip("write_then_read_roundtrip_is_byte_exact");
    };
    let key = k("acme", "markdown/skills/customer.md");
    // Trailing newline included on purpose. Routing content through
    // COPY ... TO (FORMAT csv) — the obvious-looking way to write bytes
    // from SQL — appends one, so every write would grow the file and
    // content hashes would drift. write_blob does not.
    let body = Bytes::from_static(b"---\ntype: skill\n---\n# customer\n");

    store.write(&key, body.clone()).await.expect("write");
    assert_eq!(store.read(&key).await.expect("read"), body);
}

#[tokio::test]
async fn overwrite_with_shorter_content_truncates() {
    let Some((store, _dir)) = store_and_dir() else {
        return skip("overwrite_with_shorter_content_truncates");
    };
    let key = k("acme", "skills/customer.md");

    store
        .write(&key, Bytes::from_static(b"a much longer original body"))
        .await
        .expect("first write");
    store
        .write(&key, Bytes::from_static(b"short"))
        .await
        .expect("second write");

    // Opening for write WITHOUT truncation would leave the tail of the
    // first body in place, so this reads back "shortnger original body".
    assert_eq!(
        store.read(&key).await.expect("read"),
        Bytes::from_static(b"short")
    );
    assert_eq!(store.size(&key).await.expect("size"), 5);
}

#[tokio::test]
async fn binary_content_survives_including_nul_bytes() {
    let Some((store, _dir)) = store_and_dir() else {
        return skip("binary_content_survives_including_nul_bytes");
    };
    let key = k("acme", "blobs/scan.pdf");
    // Blob storage rides these same primitives (put_blob/get_blob default
    // to read/write), so a text-only write path would corrupt every
    // uploaded PDF and image.
    let body = Bytes::from_static(&[0x00, 0x01, 0xFF, 0x00, 0x25, 0x50, 0x44, 0x46]);

    store.write(&key, body.clone()).await.expect("write");
    assert_eq!(store.read(&key).await.expect("read"), body);
}

#[tokio::test]
async fn empty_body_is_a_real_zero_length_object() {
    let Some((store, _dir)) = store_and_dir() else {
        return skip("empty_body_is_a_real_zero_length_object");
    };
    let key = k("acme", "empty.md");

    store.write(&key, Bytes::new()).await.expect("write");
    assert_eq!(store.size(&key).await.expect("size"), 0);
    assert_eq!(store.read(&key).await.expect("read"), Bytes::new());
}

#[tokio::test]
async fn missing_key_reports_not_found_not_a_generic_io_error() {
    let Some((store, _dir)) = store_and_dir() else {
        return skip("missing_key_reports_not_found_not_a_generic_io_error");
    };
    let key = k("acme", "does/not/exist.md");

    // This classification matters more here than in the other backends.
    // DuckDB surfaces a missing file as an untyped IO error, so it has to
    // be recovered from the message; getting it wrong means callers see a
    // hard error where they expect an absent key.
    assert!(
        matches!(store.read(&key).await, Err(StoreError::NotFound(_))),
        "read of an absent key must be NotFound"
    );
    assert!(
        matches!(store.size(&key).await, Err(StoreError::NotFound(_))),
        "size of an absent key must be NotFound"
    );
    assert!(
        matches!(store.delete(&key).await, Err(StoreError::NotFound(_))),
        "delete of an absent key must be NotFound"
    );
}

#[tokio::test]
async fn delete_removes_then_reports_not_found() {
    let Some((store, _dir)) = store_and_dir() else {
        return skip("delete_removes_then_reports_not_found");
    };
    let key = k("acme", "skills/gone.md");

    store
        .write(&key, Bytes::from_static(b"bye"))
        .await
        .expect("write");
    store.delete(&key).await.expect("first delete removes it");
    assert!(
        matches!(store.delete(&key).await, Err(StoreError::NotFound(_))),
        "second delete is NotFound, not success and not a raised error"
    );
}

#[tokio::test]
async fn list_is_recursive_prefix_scoped_and_tenant_isolated() {
    let Some((store, _dir)) = store_and_dir() else {
        return skip("list_is_recursive_prefix_scoped_and_tenant_isolated");
    };
    for (tenant, path) in [
        ("acme", "skills/a.md"),
        ("acme", "skills/nested/deep/b.md"),
        ("acme", "instances/c.md"),
        ("other", "skills/d.md"),
    ] {
        store
            .write(&k(tenant, path), Bytes::from_static(b"x"))
            .await
            .expect("seed write");
    }

    let under_skills = store.list(&k("acme", "skills/")).await.expect("list");
    let mut paths: Vec<_> = under_skills
        .iter()
        .map(|key| key.path().to_owned())
        .collect();
    paths.sort();

    // Recursion is on `**`; a non-recursive glob would miss the nested hit.
    assert_eq!(paths, vec!["skills/a.md", "skills/nested/deep/b.md"]);
    // The prefix filter must exclude the sibling subtree...
    assert!(!paths.iter().any(|p| p.contains("instances")));
    // ...and listing must never cross a tenant boundary.
    assert!(under_skills.iter().all(|key| key.tenant() == "acme"));
}

#[tokio::test]
async fn list_of_an_unknown_tenant_is_empty_not_an_error() {
    let Some((store, _dir)) = store_and_dir() else {
        return skip("list_of_an_unknown_tenant_is_empty_not_an_error");
    };
    // glob against a directory that was never created raises on some
    // filesystems and returns nothing on others; the trait specifies an
    // empty vec either way.
    let listed = store
        .list(&k("never-seen", ""))
        .await
        .expect("list of an unknown tenant succeeds");
    assert!(listed.is_empty());
}

#[tokio::test]
async fn write_returns_a_content_addressed_version() {
    let Some((store, _dir)) = store_and_dir() else {
        return skip("write_returns_a_content_addressed_version");
    };
    let key = k("acme", "v.md");

    let v1 = store
        .write(&key, Bytes::from_static(b"one"))
        .await
        .expect("w1");
    let v2 = store
        .write(&key, Bytes::from_static(b"two"))
        .await
        .expect("w2");
    let v3 = store
        .write(&key, Bytes::from_static(b"one"))
        .await
        .expect("w3");

    assert_ne!(v1, v2, "different bytes must produce different versions");
    // Documented divergence from FsStore/GcsStore, pinned so it is a
    // decision rather than a surprise: this Version identifies CONTENT, so
    // rewriting the same bytes reproduces the earlier version.
    assert_eq!(v1, v3, "identical bytes reproduce the same version");
    assert!(v1.starts_with("sha256:"));
}

#[tokio::test]
async fn blob_helpers_ride_the_same_primitives() {
    let Some((store, _dir)) = store_and_dir() else {
        return skip("blob_helpers_ride_the_same_primitives");
    };
    // put_blob/get_blob are trait defaults over read/write, so they are
    // free — but only if write is genuinely binary-safe. Worth one check.
    let body = Bytes::from_static(&[0xDE, 0xAD, 0x00, 0xBE, 0xEF]);
    let id = store
        .put_blob("acme", body.clone(), None)
        .await
        .expect("put_blob");
    assert_eq!(store.get_blob("acme", &id).await.expect("get_blob"), body);

    let listed = store.list_blobs("acme").await.expect("list_blobs");
    assert!(listed.contains(&id), "stored blob appears in list_blobs");
}
