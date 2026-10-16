//! Integration tests for [`escurel_storage::S3Store`].
//!
//! A real S3-compatible server over real HTTP via a `testcontainers` container — no
//! mocks, no in-memory S3 fake (principle 2: this is the named
//! component the merge gate must exercise against a real backend).
//!
//! Each test boots a fresh S3 server container, creates a bucket, and
//! mirrors the `FsStore` scenarios from `fs_roundtrip.rs` so the two
//! backends are held to the same `LaneStore` contract.

#![cfg(feature = "test-s3-server")]

use bytes::Bytes;
use escurel_storage::testing::S3Server;
use escurel_storage::{Key, LaneStore, S3Store, StoreError};

mod conformance;

/// Boot a real S3-compatible server and build an `S3Store` against it under
/// the given prefix, creating the bucket first. The server handle is returned
/// alongside the store so the caller keeps it alive for the test's duration
/// (drop tears the container down). Which server, and why: see
/// `escurel_storage::testing`.
async fn store_and_server(prefix: &str) -> (S3Store, S3Server) {
    let server = S3Server::start().await;
    let store = server.store("escurel-test", prefix).await;
    (store, server)
}

fn k(tenant: &str, path: &str) -> Key {
    Key::new(tenant.to_owned(), path.to_owned())
        .unwrap_or_else(|err| panic!("test fixture key ({tenant:?}, {path:?}): {err}"))
}

#[tokio::test]
async fn s3_write_then_read_roundtrip() {
    let (store, _node) = store_and_server("p1").await;
    let key = k("acme", "markdown/skills/customer.md");
    let body = Bytes::from_static(b"---\nkind: skill\nid: customer\n---\n# customer\n");

    store
        .write(&key, body.clone())
        .await
        .expect("write succeeds");
    let read_back = store.read(&key).await.expect("read succeeds");

    assert_eq!(read_back, body, "round-trip preserves bytes exactly");
}

#[tokio::test]
async fn s3_overwrite_replaces_bytes() {
    let (store, _node) = store_and_server("p2").await;
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
}

#[tokio::test]
async fn s3_read_missing_returns_not_found() {
    let (store, _node) = store_and_server("p3").await;
    let key = k("acme", "does/not/exist.md");

    let err = store.read(&key).await.expect_err("missing read");
    assert!(
        matches!(err, StoreError::NotFound(_)),
        "expected NotFound, got: {err:?}",
    );
}

#[tokio::test]
async fn s3_delete_then_read_returns_not_found() {
    let (store, _node) = store_and_server("p4").await;
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
async fn s3_list_returns_keys_under_prefix() {
    let (store, _node) = store_and_server("p5").await;
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
        "list returns keys whose path starts with the prefix",
    );
}

#[tokio::test]
async fn s3_list_isolates_tenants() {
    let (store, _node) = store_and_server("p6").await;
    store
        .write(
            &k("acme", "markdown/skills/customer.md"),
            Bytes::from_static(b"a"),
        )
        .await
        .expect("write acme");
    store
        .write(
            &k("globex", "markdown/skills/customer.md"),
            Bytes::from_static(b"g"),
        )
        .await
        .expect("write globex");

    let acme = store.list(&k("acme", "")).await.expect("list acme");
    let globex = store.list(&k("globex", "")).await.expect("list globex");

    assert_eq!(acme.len(), 1, "acme sees only acme keys: {acme:?}");
    assert_eq!(globex.len(), 1, "globex sees only globex keys: {globex:?}");
    assert!(acme.iter().all(|key| key.tenant() == "acme"));
    assert!(globex.iter().all(|key| key.tenant() == "globex"));
}

#[tokio::test]
async fn s3_list_under_nonexistent_prefix_returns_empty() {
    let (store, _node) = store_and_server("p7").await;
    let listed = store.list(&k("acme", "no/such/dir/")).await.expect("list");
    assert!(listed.is_empty(), "list under missing prefix is empty Vec");
}

#[tokio::test]
async fn s3_url_returns_parseable_s3_url() {
    let (store, _node) = store_and_server("p8").await;
    let key = k("acme", "markdown/skills/customer.md");

    let url = store.url(&key).expect("url");
    assert_eq!(url.scheme(), "s3");
    // s3://<bucket>/<prefix>/tenants/<tenant>/<path>
    assert_eq!(url.host_str(), Some("escurel-test"));
    assert_eq!(
        url.path(),
        "/p8/tenants/acme/markdown/skills/customer.md",
        "url encodes the full object path under bucket + prefix",
    );
    // Already proven parseable by `Url::parse` since `url()` returns it.
}

/// Practical user-perspective scenario: the path a real `update_page`
/// exercises — the indexer writes a markdown instance body to
/// `tenants/<tenant>/markdown/instances/<skill>/<instance>.md` and
/// later reads it back. Round-trip must be byte-for-byte.
#[tokio::test]
async fn s3_survives_indexer_style_page_write_and_readback() {
    let (store, _node) = store_and_server("p9").await;
    let key = k("acme", "markdown/instances/customer/acme.md");
    let body = Bytes::from(
        "---\n\
         kind: instance\n\
         skill: customer\n\
         id: acme\n\
         ---\n\
         # Acme Corp\n\
         \n\
         A long-standing customer. See [the meeting notes](meeting.md).\n\
         \n\
         - Industry: manufacturing\n\
         - Tier: enterprise\n"
            .to_owned(),
    );

    let version = store.write(&key, body.clone()).await.expect("page write");
    assert!(!version.is_empty(), "write returns a non-empty version");

    let read_back = store.read(&key).await.expect("page readback");
    assert_eq!(
        read_back, body,
        "indexer-style page round-trips byte-for-byte",
    );
}

/// The shared `LaneStore` contract (`tests/conformance/`), against real
/// the S3 server. This is what closes the drift the per-backend files had: the S3
/// side was missing `delete_missing_returns_not_found` and the
/// distinct-versions check, and the blob layer had never run against S3 at
/// all despite riding on `list()`, whose contract differs most between
/// backends.
#[tokio::test]
async fn s3_store_satisfies_the_lane_store_contract() {
    let (store, _node) = store_and_server("conformance").await;
    conformance::run_lane_store_conformance(&store, "s3").await;
}
