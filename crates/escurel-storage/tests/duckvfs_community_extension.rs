//! The extension can come from the community repository, not only a file.
//!
//! `DuckVfsStoreConfig::extension_path` names a **locally built**
//! `gdrive.duckdb_extension`. That is fine on a developer's machine and
//! impossible in a container: a pod has no such file, and the path is the
//! only way to get `write_blob`/`remove_file`/`file_size` registered — they
//! live in the extension, not in DuckDB core, and every root needs them
//! whatever its scheme.
//!
//! So a `duckvfs` lane store could not be deployed at all. `extension_path:
//! None` does not help: it SKIPS the load and assumes the functions are
//! already present, which in a fresh process they never are. The failure is
//! not at boot either — the store opens happily and the first write fails
//! on a missing function.
//!
//! `gdrive` now ships in the DuckDB community repository, which is what
//! `duckvfs.rs` anticipated in a comment ("Once gdrive is in the community
//! repository this becomes INSTALL gdrive FROM community; LOAD gdrive;").
//! This is that.
//!
//! Deliberately a `file://` root over a TempDir: the DuckDB VFS dispatches
//! on the scheme, so this exercises the same load path a `gdrive://` root
//! uses while needing no Drive credential. What it proves is that the
//! extension was obtained and its functions registered — nothing about
//! Drive itself, which the live suite covers.
//!
//! Unlike the sibling tests this takes NO `ESCUREL_TEST_GDRIVE_EXTENSION`,
//! because needing a prebuilt file is the very thing being removed. It does
//! need network access to the community repository.

#![cfg(feature = "duckvfs")]

use bytes::Bytes;
use escurel_storage::{DuckVfsStore, DuckVfsStoreConfig, Key, LaneStore};
use tempfile::TempDir;

fn k(tenant: &str, path: &str) -> Key {
    Key::new(tenant.to_owned(), path.to_owned()).expect("key")
}

/// A store configured with no local extension path still writes and reads.
///
/// The round trip is the assertion, not the constructor returning `Ok`.
/// `DuckVfsStore::new` performs no I/O against the root, so a store that
/// never loaded the extension constructs perfectly and fails later — which
/// is exactly the shape that would have reached a cluster and presented as
/// a runtime error rather than a boot failure.
#[tokio::test]
async fn the_extension_is_installed_from_the_community_repository() {
    let dir = TempDir::new().expect("tempdir");
    let store = DuckVfsStore::new(&DuckVfsStoreConfig {
        root: format!("file://{}", dir.path().display()),
        extension_path: None,
        extension_repo: Some("community".to_owned()),
        drive_id: None,
        drive_scope: None,
    })
    .expect("open a store that sources its extension from the community repo");

    let key = k("acme", "notes/hello.md");
    let body = Bytes::from_static(b"# hello\n");
    // `write` is the discriminating call: it goes through `write_blob`,
    // which lives in the extension rather than DuckDB core.
    store
        .write(&key, body.clone())
        .await
        .expect("write through the community-sourced extension");

    let got = store.read(&key).await.expect("read back");
    assert_eq!(got, body, "the round trip must return the bytes written");
}
