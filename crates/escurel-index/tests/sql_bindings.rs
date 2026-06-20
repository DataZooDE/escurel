//! Integration tests for SQL-view rebuild + validate_bindings (PR-2e).
//! Real DuckDB, real FsStore, offline json_dir. No mocks.

use std::sync::Arc;

use duckdb::Connection;
use escurel_embed::{Embedder, ZeroEmbedder};
use escurel_index::backend::{SqlConnector, SqlViewBackend, SqlViewBinding};
use escurel_index::{Indexer, Migrator};
use escurel_storage::{FsStore, LaneStore};
use tempfile::TempDir;

const TENANT: &str = "acme";

fn indexer_on(store: Arc<dyn LaneStore>, db_dir: &TempDir) -> Arc<Indexer> {
    let conn = Connection::open(db_dir.path().join("escurel.duckdb")).unwrap();
    Migrator::up(&conn).unwrap();
    let embedder: Arc<dyn Embedder> = Arc::new(ZeroEmbedder::default());
    Arc::new(Indexer::new(store, embedder, conn, TENANT).unwrap())
}

fn dir_binding(dir: &str) -> SqlViewBinding {
    SqlViewBinding {
        connector: SqlConnector::JsonDir,
        attach: None,
        relation: dir.to_owned(),
        filter: None,
        project: Default::default(),
        search_text: Vec::new(),
    }
}

#[tokio::test]
async fn rebuild_reconstructs_views_from_backend_ref() {
    let store_dir = TempDir::new().unwrap();
    let data_dir = TempDir::new().unwrap();
    std::fs::write(data_dir.path().join("a.json"), br#"{"name":"Acme"}"#).unwrap();
    std::fs::write(data_dir.path().join("b.json"), br#"{"name":"Globex"}"#).unwrap();
    let store: Arc<dyn LaneStore> = Arc::new(FsStore::new(store_dir.path().to_path_buf()));

    // indexer1 materialises the instance (overlay → the shared store).
    let db1 = TempDir::new().unwrap();
    let m = {
        let indexer1 = indexer_on(Arc::clone(&store), &db1);
        SqlViewBackend::new(Arc::clone(&indexer1))
            .create_instance(
                "customers",
                &dir_binding(data_dir.path().to_str().unwrap()),
                "eu",
                "# EU",
            )
            .await
            .unwrap()
    };

    // indexer2 is a FROM-SCRATCH DuckDB over the SAME canonical corpus: the
    // view does not exist until rebuild reconstructs it from backend_ref.source.
    let db2 = TempDir::new().unwrap();
    let indexer2 = indexer_on(Arc::clone(&store), &db2);
    // Before rebuild the view is absent.
    assert!(
        indexer2.project_view(&m.view, 10).await.is_err(),
        "view must not exist before rebuild"
    );
    indexer2.rebuild().await.expect("rebuild");
    // After rebuild the view is reconstructed and queryable.
    let rows = indexer2
        .project_view(&m.view, 10)
        .await
        .expect("reconstructed view");
    assert_eq!(rows.len(), 2);
}

#[tokio::test]
async fn schema_drift_marks_binding_degraded() {
    let store_dir = TempDir::new().unwrap();
    let data_dir = TempDir::new().unwrap();
    std::fs::write(
        data_dir.path().join("a.json"),
        br#"{"name":"Acme","tier":"gold"}"#,
    )
    .unwrap();
    let store: Arc<dyn LaneStore> = Arc::new(FsStore::new(store_dir.path().to_path_buf()));
    let db = TempDir::new().unwrap();
    let indexer = indexer_on(Arc::clone(&store), &db);

    let m = SqlViewBackend::new(Arc::clone(&indexer))
        .create_instance(
            "customers",
            &dir_binding(data_dir.path().to_str().unwrap()),
            "eu",
            "# EU",
        )
        .await
        .unwrap();

    // Healthy immediately after create.
    let ok = indexer.validate_bindings().await.unwrap();
    assert_eq!(ok.len(), 1);
    assert_eq!(ok[0].status, "ok", "fresh binding should be ok: {ok:?}");

    // Drift the source schema: drop the `tier` column.
    std::fs::write(data_dir.path().join("a.json"), br#"{"name":"Acme"}"#).unwrap();
    let drifted = indexer.validate_bindings().await.unwrap();
    assert_eq!(drifted[0].status, "binding_degraded", "got {drifted:?}");

    // The current fingerprint no longer matches the one stored at create.
    let current = indexer.current_view_fingerprint(&m.view).await.unwrap();
    assert_ne!(current, m.source_schema_fingerprint);
}
