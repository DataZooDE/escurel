//! `escurel-server events-copy` and `scripts/events-copy-job.sh`, through the REAL binary, against a REAL
//! Postgres (the lake catalog AND the source of the shared events/chat/drafts tables) and a real DuckLake
//! (local DATA_PATH).
//!
//! The engine behaviour (ids, seq, lineage, idempotency, divergence, verification) is pinned in
//! `escurel-index`'s `events_copy_live`; here the operator surface is: JSON on stdout, dry run by default,
//! the stopped-writer lease on an apply, and the job script's exit codes.
//!
//! Opt-in: behind the `live-ducklake` feature (needs Docker). Run with
//! `cargo test -p escurel-server --features live-ducklake --test suite events_copy_cmd`.

#![cfg(feature = "live-ducklake")]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::Arc;

use duckdb::Connection;
use escurel_embed::{Embedder, ZeroEmbedder};
use escurel_index::snapshot::{LakeConfig, ObjectStoreSecret, WriterLease, attach_events_lake};
use escurel_index::{AppendChatMessage, Indexer, Migrator, NewEvent};
use escurel_storage::{FsStore, LaneStore};
use tempfile::TempDir;
use testcontainers_modules::postgres::Postgres;
use testcontainers_modules::testcontainers::ContainerAsync;
use testcontainers_modules::testcontainers::runners::AsyncRunner;

const TENANT: &str = "acme";

struct Env {
    _pg: ContainerAsync<Postgres>,
    dsn: String,
    lake_dir: TempDir,
    data_dir: TempDir,
    _seed_dirs: Vec<TempDir>,
}

impl Env {
    fn lake(&self) -> LakeConfig {
        LakeConfig {
            catalog_dsn: self.dsn.clone(),
            data_path: self
                .lake_dir
                .path()
                .join("data")
                .to_str()
                .unwrap()
                .to_owned(),
            object_store: ObjectStoreSecret::None,
        }
    }
}

/// A Postgres holding 3 events and 2 chat messages of `acme` in the shared tables.
async fn env() -> Env {
    let pg = Postgres::default().start().await.expect("start postgres");
    let port = pg.get_host_port_ipv4(5432).await.expect("pg port");
    let dsn = format!("host=127.0.0.1 port={port} user=postgres password=postgres dbname=postgres");
    let lake_dir = TempDir::new().unwrap();
    std::fs::create_dir_all(lake_dir.path().join("data")).unwrap();

    let store_dir = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    let store: Arc<dyn LaneStore> = Arc::new(FsStore::new(store_dir.path().to_path_buf()));
    let embedder: Arc<dyn Embedder> = Arc::new(ZeroEmbedder::default());
    let conn = Connection::open(db_dir.path().join("escurel.duckdb")).unwrap();
    Migrator::up(&conn).unwrap();
    let indexer = Indexer::new(store, embedder, conn, TENANT).unwrap();
    indexer.attach_events_pg(&dsn).await.unwrap();
    indexer.attach_drafts_pg(&dsn).await.unwrap();
    indexer.attach_chat_pg(&dsn).await.unwrap();
    for n in 1..=3 {
        indexer
            .capture_event(NewEvent {
                event_id: Some(format!("evt-{n}")),
                at: Some(format!("2026-10-01T09:0{n}:00Z")),
                source: "gmail".to_owned(),
                label_skill: "email".to_owned(),
                title: format!("event {n}"),
                ..Default::default()
            })
            .await
            .unwrap();
    }
    for i in 0..2 {
        indexer
            .append_chat_message(AppendChatMessage {
                chat_group_id: "g",
                role: "user",
                content: "hello",
                author: None,
                ts: Some(&format!("2026-10-01T10:0{i}:00Z")),
                metadata: None,
                msg_id: Some(&format!("m{i}")),
                embed: false,
            })
            .await
            .unwrap();
    }
    Env {
        _pg: pg,
        dsn,
        lake_dir,
        data_dir: TempDir::new().unwrap(),
        _seed_dirs: vec![store_dir, db_dir],
    }
}

fn server_bin() -> Command {
    use assert_cmd::cargo::CommandCargoExt as _;
    Command::cargo_bin("escurel-server").expect("locate escurel-server binary")
}

fn lake_env(cmd: &mut Command, e: &Env) {
    cmd.env("ESCUREL_TENANT", TENANT)
        .env("ESCUREL_INDEX_BACKEND", "ducklake")
        .env("ESCUREL_DUCKLAKE_CATALOG_DSN", &e.dsn)
        .env("ESCUREL_DUCKLAKE_DATA_PATH", e.lake().data_path)
        .env("ESCUREL_SERVER_DATA_DIR", e.data_dir.path());
}

fn run(e: &Env, args: &[&str]) -> Output {
    let mut cmd = server_bin();
    cmd.arg("events-copy").args(args);
    lake_env(&mut cmd, e);
    cmd.output().expect("run escurel-server events-copy")
}

fn json(out: &Output) -> serde_json::Value {
    serde_json::from_slice(&out.stdout).unwrap_or_else(|err| {
        panic!(
            "stdout is not JSON ({err}): {}\nstderr: {}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        )
    })
}

fn table<'a>(report: &'a serde_json::Value, name: &str) -> &'a serde_json::Value {
    report["tables"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["table"] == name)
        .unwrap_or_else(|| panic!("no {name} in {report}"))
}

fn lake_event_count(e: &Env) -> i64 {
    let conn = Connection::open_in_memory().unwrap();
    attach_events_lake(&conn, &e.lake()).unwrap();
    conn.query_row(
        "SELECT count(*) FROM append_lake.escurel_events WHERE label_skill = 'email'",
        [],
        |r| r.get(0),
    )
    .unwrap()
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

#[tokio::test(flavor = "multi_thread")]
async fn a_dry_run_is_the_default_and_an_apply_copies_then_a_rerun_copies_nothing() {
    let e = env().await;

    let dry = run(&e, &[]);
    assert!(
        dry.status.success(),
        "{}",
        String::from_utf8_lossy(&dry.stderr)
    );
    let report = json(&dry);
    assert_eq!(report["applied"], false, "no flag is a dry run: {report}");
    assert_eq!(table(&report, "escurel_events")["to_copy"], 3);
    assert_eq!(table(&report, "escurel_chat_messages")["to_copy"], 2);

    let done = run(&e, &["--apply"]);
    assert!(
        done.status.success(),
        "{}",
        String::from_utf8_lossy(&done.stderr)
    );
    let report = json(&done);
    assert_eq!(report["applied"], true);
    assert_eq!(table(&report, "escurel_events")["copied"], 3);
    assert_eq!(lake_event_count(&e), 3);
    assert!(report["audit_event_id"].is_string(), "{report}");

    let again = run(&e, &["--apply"]);
    assert!(again.status.success());
    let report = json(&again);
    assert_eq!(table(&report, "escurel_events")["copied"], 0);
    assert_eq!(lake_event_count(&e), 3, "no duplicates");
}

#[tokio::test(flavor = "multi_thread")]
async fn an_apply_is_refused_while_a_writer_holds_the_lease_and_copies_nothing() {
    let e = env().await;
    let _writer = WriterLease::acquire(&e.dsn)
        .await
        .unwrap()
        .expect("the test takes the lease first");

    let out = run(&e, &["--apply"]);

    assert!(
        !out.status.success(),
        "an apply against a live writer must fail"
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("single-writer lease"),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(lake_event_count(&e), 0, "nothing was copied");
    // A dry run only reads: it works beside a live writer.
    assert!(run(&e, &[]).status.success());
}

#[tokio::test(flavor = "multi_thread")]
async fn the_job_script_exits_zero_after_a_verified_copy_and_non_zero_beside_a_writer() {
    let e = env().await;
    let bin_dir = {
        use assert_cmd::cargo::CommandCargoExt as _;
        let server = Command::cargo_bin("escurel-server").unwrap();
        Path::new(server.get_program()).parent().unwrap().to_owned()
    };
    let job = |e: &Env| {
        let mut cmd = Command::new("sh");
        cmd.arg(repo_root().join("scripts/events-copy-job.sh"))
            .env("ESCUREL_BIN_DIR", &bin_dir)
            .env("ESCUREL_JOB_DEADLINE_SECS", "300");
        lake_env(&mut cmd, e);
        cmd.output().expect("run the job script")
    };

    // A live writer: the apply step fails, so the job fails.
    {
        let _writer = WriterLease::acquire(&e.dsn).await.unwrap().expect("lease");
        let out = job(&e);
        assert!(!out.status.success(), "the job must fail beside a writer");
        assert_eq!(lake_event_count(&e), 0);
    }

    let out = job(&e);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(String::from_utf8_lossy(&out.stderr).contains("events-copy-job: OK"));
    assert_eq!(lake_event_count(&e), 3);
}
