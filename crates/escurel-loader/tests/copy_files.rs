//! E2E for the files-first half of a transfer: `copy_files` moves the loader's
//! canonical blobs + overlay markdown into a live tenant's LaneStore. Real
//! FsStores, no mocks. Asserts byte-equal blob content, the overlays re-keyed
//! under the live tenant, and idempotent re-runs.

use std::sync::Arc;

use escurel_embed::{Embedder, HashEmbedder};
use escurel_index::backend::{Extractor, PlainTextExtractor};
use escurel_loader::{LoaderBuilder, copy_files};
use escurel_storage::{FsStore, Key, LaneStore};
use tempfile::TempDir;

#[tokio::test]
async fn copy_files_moves_blobs_and_overlays_into_live_tenant_idempotently() {
    // Build a 2-doc loader artifact.
    let src = TempDir::new().unwrap();
    std::fs::write(src.path().join("a.txt"), "alpha note").unwrap();
    std::fs::write(src.path().join("b.txt"), "beta note").unwrap();
    let loader_dir = TempDir::new().unwrap();
    let extractor: Arc<dyn Extractor> = Arc::new(PlainTextExtractor);
    let embedder: Arc<dyn Embedder> = Arc::new(HashEmbedder::default());
    LoaderBuilder::new(loader_dir.path(), "attachment", extractor, embedder)
        .build(src.path())
        .await
        .expect("loader build");

    // Copy into a fresh live store + tenant.
    let live_root = TempDir::new().unwrap();
    let live: Arc<dyn LaneStore> = Arc::new(FsStore::new(live_root.path().to_path_buf()));
    let report = copy_files(loader_dir.path(), live.as_ref(), "acme")
        .await
        .expect("copy_files");

    assert_eq!(report.blobs, 2, "two canonical blobs copied");
    assert_eq!(report.overlays, 2, "two overlay markdown files copied");

    // The live tenant now holds both blobs, byte-equal to the loader's.
    let loader_store: Arc<dyn LaneStore> = Arc::new(FsStore::new(loader_dir.path().to_path_buf()));
    let live_blobs = live.list_blobs("acme").await.unwrap();
    assert_eq!(live_blobs.len(), 2);
    for id in &live_blobs {
        let got = live.get_blob("acme", id).await.unwrap();
        let want = loader_store.get_blob("loader", id).await.unwrap();
        assert_eq!(got, want, "blob {id:?} byte-equal across stores");
    }

    // Overlays landed under the live tenant at the same paths.
    let overlays = live
        .list(&Key::new("acme", "markdown").unwrap())
        .await
        .unwrap();
    assert_eq!(overlays.len(), 2);
    assert!(
        overlays
            .iter()
            .all(|k| k.tenant() == "acme" && k.path().starts_with("markdown/instances/")),
        "overlays re-keyed under acme: {overlays:?}"
    );

    // Idempotent: a second copy (crash-retry) succeeds with the same tallies and
    // does not duplicate blobs.
    let again = copy_files(loader_dir.path(), live.as_ref(), "acme")
        .await
        .expect("copy_files re-run");
    assert_eq!(again.blobs, 2);
    assert_eq!(again.overlays, 2);
    assert_eq!(
        live.list_blobs("acme").await.unwrap().len(),
        2,
        "no duplicate blobs"
    );
}
