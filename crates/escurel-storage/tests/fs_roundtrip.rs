//! Integration tests for [`escurel_storage::FsStore`].
//!
//! Real filesystem (`tempfile::TempDir`), no mocks. Each test
//! constructs a fresh tempdir and asserts behaviour on real files.

use bytes::Bytes;
use escurel_storage::{FsStore, Key, LaneStore, StoreError};
use tempfile::TempDir;

fn store_and_dir() -> (FsStore, TempDir) {
    let dir = TempDir::new().expect("tempdir");
    let store = FsStore::new(dir.path().to_path_buf());
    (store, dir)
}

fn k(tenant: &str, path: &str) -> Key {
    Key::new(tenant.to_owned(), path.to_owned())
}

#[tokio::test]
async fn write_then_read_roundtrip() {
    let (store, _dir) = store_and_dir();
    let key = k("acme", "markdown/skills/customer.md");
    let body = Bytes::from_static(b"---\ntype: skill\nid: customer\n---\n# customer\n");

    store
        .write(&key, body.clone())
        .await
        .expect("write succeeds");
    let read_back = store.read(&key).await.expect("read succeeds");

    assert_eq!(read_back, body, "round-trip preserves bytes exactly");
}

#[tokio::test]
async fn write_creates_parent_directories() {
    let (store, dir) = store_and_dir();
    let key = k("acme", "markdown/instances/customer/acme-corp.md");
    let body = Bytes::from_static(b"---\ntype: instance\nskill: customer\n---\n");

    store
        .write(&key, body)
        .await
        .expect("nested write succeeds");

    let on_disk = dir
        .path()
        .join("tenants")
        .join("acme")
        .join("markdown/instances/customer/acme-corp.md");
    assert!(on_disk.exists(), "nested file landed at expected path");
}

#[tokio::test]
async fn overwrite_replaces_bytes_and_leaves_no_tmp_orphan() {
    let (store, dir) = store_and_dir();
    let key = k("acme", "markdown/skills/customer.md");

    store
        .write(&key, Bytes::from_static(b"v1"))
        .await
        .expect("first write");
    store
        .write(&key, Bytes::from_static(b"v2-much-longer-than-before"))
        .await
        .expect("second write");

    let read_back = store.read(&key).await.expect("read");
    assert_eq!(read_back, Bytes::from_static(b"v2-much-longer-than-before"));

    // No `.tmp` orphan should remain alongside the published file.
    let parent = dir.path().join("tenants/acme/markdown/skills");
    let tmp_orphans: Vec<_> = std::fs::read_dir(&parent)
        .expect("parent dir exists")
        .filter_map(std::result::Result::ok)
        .filter(|e| {
            e.path()
                .extension()
                .and_then(|ext| ext.to_str())
                .is_some_and(|ext| ext.ends_with("tmp"))
        })
        .collect();
    assert!(
        tmp_orphans.is_empty(),
        "no .tmp orphans, found: {tmp_orphans:?}",
    );
}

#[tokio::test]
async fn read_missing_returns_not_found() {
    let (store, _dir) = store_and_dir();
    let key = k("acme", "does/not/exist.md");

    let err = store.read(&key).await.expect_err("missing read");
    assert!(
        matches!(err, StoreError::NotFound(_)),
        "expected NotFound, got: {err:?}",
    );
}

#[tokio::test]
async fn delete_then_read_returns_not_found() {
    let (store, _dir) = store_and_dir();
    let key = k("acme", "tmp.md");

    store
        .write(&key, Bytes::from_static(b"x"))
        .await
        .expect("write");
    store.delete(&key).await.expect("delete");

    let err = store.read(&key).await.expect_err("post-delete read");
    assert!(matches!(err, StoreError::NotFound(_)));
}

#[tokio::test]
async fn delete_missing_returns_not_found() {
    let (store, _dir) = store_and_dir();
    let key = k("acme", "never-existed.md");

    let err = store.delete(&key).await.expect_err("delete missing");
    assert!(matches!(err, StoreError::NotFound(_)));
}

#[tokio::test]
async fn list_returns_keys_under_prefix() {
    let (store, _dir) = store_and_dir();
    let keys = [
        k("acme", "markdown/skills/customer.md"),
        k("acme", "markdown/skills/meeting.md"),
        k("acme", "markdown/instances/customer/acme-corp.md"),
        k("acme", "manifest.toml"),
    ];
    for key in &keys {
        store
            .write(key, Bytes::from_static(b"x"))
            .await
            .expect("write");
    }

    let mut listed = store
        .list(&k("acme", "markdown/skills/"))
        .await
        .expect("list");
    listed.sort_by(|a, b| a.path().cmp(b.path()));

    assert_eq!(
        listed,
        vec![
            k("acme", "markdown/skills/customer.md"),
            k("acme", "markdown/skills/meeting.md"),
        ],
        "list returns keys whose path starts with the prefix; \
         other tenants' siblings excluded",
    );
}

#[tokio::test]
async fn list_under_non_existent_prefix_returns_empty() {
    let (store, _dir) = store_and_dir();
    let listed = store.list(&k("acme", "no/such/dir/")).await.expect("list");
    assert!(listed.is_empty(), "list under missing prefix is empty Vec");
}

#[tokio::test]
async fn list_isolates_tenants() {
    let (store, _dir) = store_and_dir();
    store
        .write(
            &k("acme", "markdown/skills/customer.md"),
            Bytes::from_static(b"a"),
        )
        .await
        .expect("write");
    store
        .write(
            &k("globex", "markdown/skills/customer.md"),
            Bytes::from_static(b"g"),
        )
        .await
        .expect("write");

    let acme = store.list(&k("acme", "")).await.expect("list acme");
    let globex = store.list(&k("globex", "")).await.expect("list globex");

    assert_eq!(acme.len(), 1, "acme sees only acme keys: {acme:?}");
    assert_eq!(globex.len(), 1, "globex sees only globex keys: {globex:?}");
    assert!(acme.iter().all(|k| k.tenant() == "acme"));
    assert!(globex.iter().all(|k| k.tenant() == "globex"));
}

#[tokio::test]
async fn url_returns_parseable_file_url() {
    let (store, dir) = store_and_dir();
    let key = k("acme", "markdown/skills/customer.md");

    let url = store.url(&key).expect("url");
    assert_eq!(url.scheme(), "file");

    let url_path = url.to_file_path().expect("file URL parses back to path");
    let expected = dir.path().join("tenants/acme/markdown/skills/customer.md");
    assert_eq!(
        url_path, expected,
        "url round-trips to the right filesystem path"
    );
}

#[tokio::test]
async fn write_returns_distinct_versions_for_distinct_writes() {
    let (store, _dir) = store_and_dir();
    let key = k("acme", "v.md");

    let v1 = store
        .write(&key, Bytes::from_static(b"one"))
        .await
        .expect("write 1");
    // Sleep enough to clear nanosecond mtime resolution on any
    // reasonable filesystem. `std::thread::sleep` is fine here —
    // we want to advance wall-clock time, not yield to the runtime.
    std::thread::sleep(std::time::Duration::from_millis(20));
    let v2 = store
        .write(&key, Bytes::from_static(b"two"))
        .await
        .expect("write 2");

    assert_ne!(
        v1, v2,
        "two successive writes must produce different versions"
    );
}
