//! `escurel-server lake-upgrade`: the supported lake schema upgrade, through the REAL binary against a
//! REAL DuckLake (DuckDB-file catalog + local DATA_PATH).
//!
//! The cutover of 2026-10-13 died at boot with `lake schema_version 9 != reader schema_version 10`
//! and the only way across was a hand-run `UPDATE lake.escurel_manifest`. The command is what the
//! migration job (and an operator) runs instead: JSON on stdout, exit 0 only when the lake is at
//! (or was moved to) this build's version, non-zero for an unknown jump or a newer lake.

use std::process::{Command, Output};

use duckdb::Connection;
use escurel_index::Migrator;
use escurel_index::snapshot::{LakeConfig, ObjectStoreSecret, attach_lake};
use tempfile::TempDir;

fn cargo_bin() -> Command {
    use assert_cmd::cargo::CommandCargoExt as _;
    Command::cargo_bin("escurel-server").expect("locate escurel-server binary")
}

struct Lake {
    cfg: LakeConfig,
    _dir: TempDir,
}

/// A lake whose manifest pins `version` (the table the command reads and pins; the corpus tables are
/// not needed for a manifest-only upgrade).
fn lake_at(version: i64) -> Lake {
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
    let conn = Connection::open_in_memory().unwrap();
    attach_lake(&conn, &cfg, false).unwrap();
    conn.execute_batch(&format!(
        "CREATE TABLE lake.escurel_manifest AS SELECT {version}::INTEGER AS schema_version, \
         'hash'::VARCHAR AS model_id, 768::INTEGER AS dim, 'test'::VARCHAR AS escurel_version, \
         0::BIGINT AS pages, 0::BIGINT AS blocks, 0::BIGINT AS published_epoch;"
    ))
    .unwrap();
    Lake { cfg, _dir: dir }
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

fn run(lake: &Lake, args: &[&str]) -> Output {
    cargo_bin()
        .arg("lake-upgrade")
        .args(args)
        .env("ESCUREL_INDEX_BACKEND", "ducklake")
        .env("ESCUREL_DUCKLAKE_CATALOG_DSN", &lake.cfg.catalog_dsn)
        .env("ESCUREL_DUCKLAKE_DATA_PATH", &lake.cfg.data_path)
        .env(
            "ESCUREL_SERVER_DATA_DIR",
            lake._dir.path().join("server-data"),
        )
        .output()
        .expect("run escurel-server lake-upgrade")
}

fn json(out: &Output) -> serde_json::Value {
    serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "stdout is not JSON ({e}): {}",
            String::from_utf8_lossy(&out.stdout)
        )
    })
}

fn behind() -> i64 {
    i64::from(*Migrator::LAKE_ADOPTS_FROM.last().unwrap())
}

#[test]
fn upgrades_a_compatible_older_lake_and_a_second_run_changes_nothing() {
    let lake = lake_at(behind());

    let dry = run(&lake, &["--dry-run"]);
    assert!(
        dry.status.success(),
        "{}",
        String::from_utf8_lossy(&dry.stderr)
    );
    assert_eq!(json(&dry)["action"], "would_upgrade");
    assert_eq!(
        manifest_version(&lake.cfg),
        behind(),
        "--dry-run must not write"
    );

    let done = run(&lake, &[]);
    assert!(
        done.status.success(),
        "{}",
        String::from_utf8_lossy(&done.stderr)
    );
    let report = json(&done);
    assert_eq!(report["action"], "upgraded");
    assert_eq!(report["from"], behind());
    assert_eq!(report["to"], i64::from(Migrator::SCHEMA_VERSION));
    assert_eq!(
        manifest_version(&lake.cfg),
        i64::from(Migrator::SCHEMA_VERSION)
    );

    let again = run(&lake, &[]);
    assert!(again.status.success());
    assert_eq!(json(&again)["action"], "already_current");
}

#[test]
fn refuses_an_unknown_jump_and_a_newer_lake_with_a_non_zero_exit_and_no_write() {
    for v in [0_i64, i64::from(Migrator::SCHEMA_VERSION) + 1] {
        let lake = lake_at(v);
        let out = run(&lake, &[]);
        assert!(!out.status.success(), "version {v} must be refused");
        assert!(
            String::from_utf8_lossy(&out.stderr).contains("cannot be upgraded"),
            "stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(
            manifest_version(&lake.cfg),
            v,
            "a refused upgrade must not touch the lake"
        );
    }
}

#[test]
fn needs_a_lake_configuration() {
    let out = cargo_bin()
        .arg("lake-upgrade")
        .env_remove("ESCUREL_INDEX_BACKEND")
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("ESCUREL_INDEX_BACKEND=ducklake"));
}

/// The lab cutover of 2026-10-15 died on its very first step in a k8s Job: the pod's HOME and
/// `ESCUREL_SERVER_DATA_DIR` are `/tmp/escurel` on a FRESH emptyDir, nothing has ever created it
/// (only a booting server does), and DuckDB refuses to open with
/// `Can't find the home directory at '/tmp/escurel'`. The command is the first thing the job runs, so it
/// must create the data dir (and the DuckDB home) itself.
#[test]
fn upgrades_on_a_fresh_volume_where_the_data_dir_and_home_do_not_exist_yet() {
    let lake = lake_at(behind());
    let fresh = TempDir::new().unwrap();
    let data_dir = fresh.path().join("escurel");
    assert!(!data_dir.exists());

    let out = cargo_bin()
        .arg("lake-upgrade")
        .env("ESCUREL_INDEX_BACKEND", "ducklake")
        .env("ESCUREL_DUCKLAKE_CATALOG_DSN", &lake.cfg.catalog_dsn)
        .env("ESCUREL_DUCKLAKE_DATA_PATH", &lake.cfg.data_path)
        .env("ESCUREL_SERVER_DATA_DIR", &data_dir)
        .env("HOME", &data_dir)
        .output()
        .expect("run escurel-server lake-upgrade");
    assert!(
        out.status.success(),
        "a fresh volume must not break the first job step: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(json(&out)["action"], "upgraded");
    assert!(data_dir.is_dir(), "the data dir is created by the command");
}

#[test]
fn an_uncreatable_data_dir_fails_with_a_message_naming_the_env_var() {
    let lake = lake_at(behind());
    let fresh = TempDir::new().unwrap();
    // A regular file where a directory is needed: creating `<file>/escurel` must fail.
    let blocker = fresh.path().join("blocker");
    std::fs::write(&blocker, b"x").unwrap();
    let out = cargo_bin()
        .arg("lake-upgrade")
        .env("ESCUREL_INDEX_BACKEND", "ducklake")
        .env("ESCUREL_DUCKLAKE_CATALOG_DSN", &lake.cfg.catalog_dsn)
        .env("ESCUREL_DUCKLAKE_DATA_PATH", &lake.cfg.data_path)
        .env("ESCUREL_SERVER_DATA_DIR", blocker.join("escurel"))
        .output()
        .expect("run escurel-server lake-upgrade");
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("ESCUREL_SERVER_DATA_DIR"),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// The reverse of the one-way step, for a compatible bump ONLY: after a failed cutover the previous image refuses
/// a lake pinned at this build's schema, and `--to <previous>` restores the pin so it boots again.
#[test]
fn to_restores_the_previous_compatible_version_idempotently_and_upgrade_goes_forward_again() {
    let lake = lake_at(i64::from(Migrator::SCHEMA_VERSION));
    let to = behind().to_string();

    let dry = run(&lake, &["--to", &to, "--dry-run"]);
    assert!(
        dry.status.success(),
        "{}",
        String::from_utf8_lossy(&dry.stderr)
    );
    assert_eq!(json(&dry)["action"], "would_downgrade");
    assert_eq!(
        manifest_version(&lake.cfg),
        i64::from(Migrator::SCHEMA_VERSION),
        "--dry-run must not write"
    );

    let done = run(&lake, &["--to", &to]);
    assert!(
        done.status.success(),
        "{}",
        String::from_utf8_lossy(&done.stderr)
    );
    let report = json(&done);
    assert_eq!(report["action"], "downgraded");
    assert_eq!(report["from"], i64::from(Migrator::SCHEMA_VERSION));
    assert_eq!(report["to"], behind());
    assert_eq!(manifest_version(&lake.cfg), behind());

    let again = run(&lake, &["--to", &to]);
    assert!(again.status.success());
    assert_eq!(json(&again)["action"], "already_current");

    // ... and the plain upgrade moves it forward again.
    let up = run(&lake, &[]);
    assert!(up.status.success());
    assert_eq!(json(&up)["action"], "upgraded");
    assert_eq!(
        manifest_version(&lake.cfg),
        i64::from(Migrator::SCHEMA_VERSION)
    );
}

#[test]
fn to_refuses_a_target_that_is_not_a_declared_compatible_version_and_writes_nothing() {
    let lake = lake_at(i64::from(Migrator::SCHEMA_VERSION));
    for to in ["0", "1", "11", "abc", "-3"] {
        let out = run(&lake, &["--to", to]);
        assert!(!out.status.success(), "--to {to} must be refused");
        assert_eq!(
            manifest_version(&lake.cfg),
            i64::from(Migrator::SCHEMA_VERSION),
            "a refused --to {to} must not touch the lake"
        );
    }
    // A lake at an UNKNOWN version is not walked back either.
    let odd = lake_at(0);
    let out = run(&odd, &["--to", &behind().to_string()]);
    assert!(!out.status.success());
    assert_eq!(manifest_version(&odd.cfg), 0);
}
