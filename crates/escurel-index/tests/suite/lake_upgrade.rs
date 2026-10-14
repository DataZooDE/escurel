//! The supported lake schema upgrade (`lake_upgrade`) and the compatible-bump acceptance gate.
//!
//! The lab cutover of 2026-10-13 died at boot with `lake schema_version 9 != reader schema_version 10`
//! (the 9 -> 10 bump only added `events.revision_binding_sha256`, which the events attach migrates
//! itself): the only way across was a hand-run `UPDATE lake.escurel_manifest`. These tests pin the
//! supported path against a real DuckLake (DuckDB-file catalog + local DATA_PATH, no mocks).

use std::sync::Arc;

use duckdb::Connection;
use escurel_embed::{Embedder, HashEmbedder};
use escurel_index::snapshot::{
    LakeConfig, LakeUpgradeAction, ObjectStoreSecret, SnapshotError, adopt_lake, attach_lake,
    lake_upgrade, publish_lake,
};
use escurel_index::{Indexer, Migrator};
use escurel_storage::{FsStore, LaneStore};
use tempfile::TempDir;

const TENANT: &str = "acme";

const SKILL: (&str, &str) = (
    "markdown/skills/customer.md",
    "---\nkind: skill\nid: customer\ndescription: a customer\n---\n# customer\n\nA customer.\n",
);

struct Lake {
    store: Arc<dyn LaneStore>,
    indexer: Arc<Indexer>,
    cfg: LakeConfig,
    _dirs: Vec<TempDir>,
}

async fn published_lake() -> Lake {
    let store_dir = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    let lake_dir = TempDir::new().unwrap();
    std::fs::create_dir_all(lake_dir.path().join("data")).unwrap();
    let store: Arc<dyn LaneStore> = Arc::new(FsStore::new(store_dir.path().to_path_buf()));
    let embedder: Arc<dyn Embedder> = Arc::new(HashEmbedder::default());
    let conn = Connection::open(db_dir.path().join("escurel.duckdb")).unwrap();
    Migrator::up(&conn).unwrap();
    let indexer = Arc::new(Indexer::new(Arc::clone(&store), embedder, conn, TENANT).unwrap());
    indexer.update_page(SKILL.0, SKILL.1).await.unwrap();
    let cfg = LakeConfig {
        catalog_dsn: lake_dir
            .path()
            .join("catalog.ducklake")
            .to_str()
            .unwrap()
            .to_owned(),
        data_path: lake_dir.path().join("data").to_str().unwrap().to_owned(),
        object_store: ObjectStoreSecret::None,
    };
    publish_lake(&indexer, &cfg, None).await.expect("publish");
    Lake {
        store,
        indexer,
        cfg,
        _dirs: vec![store_dir, db_dir, lake_dir],
    }
}

fn set_manifest_version(cfg: &LakeConfig, v: i64) {
    let conn = Connection::open_in_memory().unwrap();
    attach_lake(&conn, cfg, false).unwrap();
    conn.execute_batch(&format!(
        "UPDATE lake.escurel_manifest SET schema_version = {v};"
    ))
    .unwrap();
}

fn manifest_version(cfg: &LakeConfig) -> i64 {
    let conn = Connection::open_in_memory().unwrap();
    attach_lake(&conn, cfg, true).unwrap();
    conn.query_row(
        "SELECT schema_version FROM lake.escurel_manifest",
        [],
        |r| r.get(0),
    )
    .unwrap()
}

fn embedder() -> Arc<dyn Embedder> {
    Arc::new(HashEmbedder::default())
}

fn behind() -> i64 {
    i64::from(
        *Migrator::LAKE_ADOPTS_FROM
            .last()
            .expect("at least one accepted older version"),
    )
}

#[tokio::test]
async fn a_lake_one_declared_compatible_version_behind_adopts_without_any_manual_step() {
    let l = published_lake().await;
    set_manifest_version(&l.cfg, behind());
    let adopted = adopt_lake(&l.cfg, Arc::clone(&l.store), embedder(), TENANT, None)
        .await
        .expect("a declared-compatible older lake must adopt as-is (boot must not need a hand-run UPDATE)")
        .expect("indexer");
    assert!(adopted.indexer.resolve("[[customer]]", None).await.is_ok());
    let _ = &l.indexer;
}

#[tokio::test]
async fn lake_upgrade_pins_the_manifest_and_a_second_run_is_a_no_op() {
    let l = published_lake().await;
    set_manifest_version(&l.cfg, behind());

    let dry = lake_upgrade(&l.cfg, true).await.expect("dry run");
    assert_eq!(dry.action, LakeUpgradeAction::WouldUpgrade);
    assert_eq!(
        (dry.from, dry.to),
        (behind(), i64::from(Migrator::SCHEMA_VERSION))
    );
    assert_eq!(
        manifest_version(&l.cfg),
        behind(),
        "a dry run must not write"
    );

    let done = lake_upgrade(&l.cfg, false).await.expect("upgrade");
    assert_eq!(done.action, LakeUpgradeAction::Upgraded);
    assert_eq!(
        manifest_version(&l.cfg),
        i64::from(Migrator::SCHEMA_VERSION)
    );

    let again = lake_upgrade(&l.cfg, false).await.expect("idempotent");
    assert_eq!(again.action, LakeUpgradeAction::AlreadyCurrent);
}

#[tokio::test]
async fn lake_upgrade_refuses_unknown_jumps_and_a_newer_lake_and_writes_nothing() {
    let l = published_lake().await;
    for v in [0_i64, i64::from(Migrator::SCHEMA_VERSION) + 1] {
        set_manifest_version(&l.cfg, v);
        let err = lake_upgrade(&l.cfg, false).await.expect_err("must refuse");
        assert!(
            matches!(err, SnapshotError::LakeIncompatible(_)),
            "got {err}"
        );
        assert_eq!(
            manifest_version(&l.cfg),
            v,
            "a refused upgrade must not touch the manifest"
        );
    }
}

#[tokio::test]
async fn lake_upgrade_on_a_lake_that_was_never_published_says_so() {
    let dir = TempDir::new().unwrap();
    std::fs::create_dir_all(dir.path().join("data")).unwrap();
    let cfg = LakeConfig {
        catalog_dsn: dir
            .path()
            .join("catalog.ducklake")
            .to_str()
            .unwrap()
            .to_owned(),
        data_path: dir.path().join("data").to_str().unwrap().to_owned(),
        object_store: ObjectStoreSecret::None,
    };
    let r = lake_upgrade(&cfg, false)
        .await
        .expect("an empty lake is not an error");
    assert_eq!(r.action, LakeUpgradeAction::NoLake);
}
