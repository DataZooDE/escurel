//! Mechanical enforcement of substrate SPEC §5's residency invariant
//! (#306 / #309): after real write round-trips against a REAL Postgres
//! catalog, **no customer bytes may be present in any catalog table**.
//!
//! The catalog may hold metadata only; customer payload belongs on the
//! object store (`DATA_PATH`). Two independent doors could violate it:
//!
//! 1. **inlining** — DuckLake's `DATA_INLINING_ROW_LIMIT` puts small
//!    inserts back into catalog rows (`data_inlining: false` exists to
//!    prevent exactly this, and every `ATTACH` here pins the limit to
//!    `0`);
//! 2. **attached tables** — an append surface (chat, events) attached
//!    as a plain Postgres table stores its rows IN the catalog
//!    database. That is the shape PRs #295–#297 chose and #308 walked
//!    back for chat + events (`AppendBackend::DuckLake` puts them on
//!    the lake). `crdt_ops`/`crdt_snapshots` REMAIN a sanctioned
//!    exception — they need an enforced PRIMARY KEY the lake cannot
//!    provide — recorded in substrate ADR-0015 §O7 / SPEC §5, hence the
//!    allowlist below.
//!
//! The test writes distinctive sentinels through every surface (pages,
//! chat, events), publishes + compacts, then dumps EVERY catalog row as
//! text and fails on any sentinel hit — after first proving the scanner
//! can see a planted sentinel at all, so a scan bug cannot pass as
//! compliance. The positive control greps the lake's DATA_PATH files
//! for the same sentinels, proving the payload really exists and really
//! lives on the object store side.
//!
//! Opt-in: gated behind the `live-ducklake` feature (needs Docker). Run
//! with `cargo test -p escurel-index --features live-ducklake --test
//! suite no_payload_in_catalog`.

#![cfg(feature = "live-ducklake")]

use std::sync::Arc;

use duckdb::Connection;
use escurel_embed::{Embedder, ZeroEmbedder};
use escurel_index::indexer::AppendChatMessage;
use escurel_index::snapshot::{LakeConfig, ObjectStoreSecret, publish_lake};
use escurel_index::{Indexer, Migrator, NewEvent};
use escurel_storage::{FsStore, LaneStore};
use tempfile::TempDir;
use testcontainers_modules::postgres::Postgres;
use testcontainers_modules::testcontainers::runners::AsyncRunner;
use tokio_postgres::NoTls;

const TENANT: &str = "acme";

// Distinctive enough that a hit can only be OUR payload.
const PAGE_SENTINEL: &str = "CUSTOMER-PAYLOAD-PAGE-9f31c2";
const CHAT_SENTINEL: &str = "CUSTOMER-PAYLOAD-CHAT-9f31c2";
const EVENT_SENTINEL: &str = "CUSTOMER-PAYLOAD-EVENT-9f31c2";
const CONTROL_SENTINEL: &str = "SCANNER-CONTROL-9f31c2";

/// The sanctioned catalog-resident tables (ADR-0015 §O7 / SPEC §5): the
/// CRDT op-log and snapshots need an enforced
/// `PRIMARY KEY (tenant, page_id, op_id)` — it is what turns a stale
/// `max_hlc` read into a loud failure instead of silent op-log
/// corruption — and the lake enforces no constraints. Everything else
/// in the catalog is metadata and must stay free of customer bytes.
const SANCTIONED_TABLES: &[&str] = &["escurel_crdt_ops", "escurel_crdt_snapshots"];

/// Every `(schema.table, row-as-text)` in the catalog that contains
/// `needle`, skipping the sanctioned CRDT tables.
async fn catalog_hits(client: &tokio_postgres::Client, needle: &str) -> Vec<String> {
    let tables = client
        .query(
            "SELECT table_schema, table_name FROM information_schema.tables \
             WHERE table_type = 'BASE TABLE' \
             AND table_schema NOT IN ('pg_catalog', 'information_schema')",
            &[],
        )
        .await
        .expect("list catalog tables");
    let mut hits = Vec::new();
    for row in &tables {
        let schema: &str = row.get(0);
        let table: &str = row.get(1);
        if SANCTIONED_TABLES.contains(&table) {
            continue;
        }
        let rows = client
            .query(
                &format!("SELECT (t.*)::text FROM \"{schema}\".\"{table}\" t"),
                &[],
            )
            .await
            .expect("dump table");
        for r in &rows {
            let text: String = r.get(0);
            if text.contains(needle) {
                hits.push(format!("{schema}.{table}: {text}"));
            }
        }
    }
    hits
}

/// Every file under `dir` (recursively) whose bytes contain `needle`.
fn files_containing(dir: &std::path::Path, needle: &str) -> Vec<std::path::PathBuf> {
    let mut found = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for entry in std::fs::read_dir(&d).expect("read data dir") {
            let path = entry.expect("dir entry").path();
            if path.is_dir() {
                stack.push(path);
            } else if std::fs::read(&path)
                .map(|bytes| bytes.windows(needle.len()).any(|w| w == needle.as_bytes()))
                .unwrap_or(false)
            {
                found.push(path);
            }
        }
    }
    found
}

#[tokio::test]
async fn no_customer_bytes_land_in_the_catalog() {
    let pg = Postgres::default().start().await.expect("start postgres");
    let port = pg.get_host_port_ipv4(5432).await.expect("pg port");
    let dsn = format!("host=127.0.0.1 port={port} user=postgres password=postgres dbname=postgres");

    // Local-dir DATA_PATH: same ducklake write path as s3://, and the
    // positive control can grep the parquet bytes directly.
    let lake_dir = TempDir::new().unwrap();
    std::fs::create_dir_all(lake_dir.path().join("data")).unwrap();
    let cfg = LakeConfig {
        catalog_dsn: dsn.clone(),
        data_path: lake_dir.path().join("data").to_str().unwrap().to_owned(),
        object_store: ObjectStoreSecret::None,
    };

    let store_dir = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    let store: Arc<dyn LaneStore> = Arc::new(FsStore::new(store_dir.path().to_path_buf()));
    let embedder: Arc<dyn Embedder> = Arc::new(ZeroEmbedder::default());
    let conn = Connection::open(db_dir.path().join("escurel.duckdb")).unwrap();
    Migrator::up(&conn).unwrap();
    let indexer = Arc::new(Indexer::new(store, embedder, conn, TENANT).unwrap());

    // Customer bytes through every surface. Pages → published corpus;
    // chat + events → the LAKE-backed append tables (#308's residency
    // answer; the Postgres-attached variant deliberately keeps rows in
    // the catalog and is not the configuration under test).
    indexer
        .update_page(
            "markdown/skills/customer.md",
            "---\ntype: skill\nid: customer\ndescription: a customer\n---\n# customer\n",
        )
        .await
        .unwrap();
    indexer
        .update_page(
            "markdown/instances/customer/acme.md",
            &format!(
                "---\ntype: instance\nskill: customer\nid: acme\n---\n# Acme\n\n{PAGE_SENTINEL}\n"
            ),
        )
        .await
        .unwrap();
    publish_lake(&indexer, &cfg, None).await.expect("publish");

    indexer.attach_chat_lake(&cfg).await.expect("chat lake");
    indexer
        .append_chat_message(AppendChatMessage {
            chat_group_id: "room-1",
            role: "user",
            content: CHAT_SENTINEL,
            author: None,
            ts: None,
            metadata: None,
            msg_id: None,
            embed: false,
        })
        .await
        .expect("append chat");

    indexer.attach_events_lake(&cfg).await.expect("events lake");
    indexer
        .capture_event(NewEvent {
            at: Some("2026-04-01T09:00:00Z".to_owned()),
            source: "gmail".to_owned(),
            mime: "text/plain".to_owned(),
            label_skill: "customer".to_owned(),
            title: "enquiry".to_owned(),
            body: EVENT_SENTINEL.to_owned(),
            provenance: None,
            ..Default::default()
        })
        .await
        .expect("capture event");

    // A second publish exercises the prune/GC pass over the now-dirty
    // append tables too.
    publish_lake(&indexer, &cfg, None).await.expect("republish");

    let (client, connection) = tokio_postgres::connect(&dsn, NoTls).await.expect("connect");
    tokio::spawn(async move {
        let _ = connection.await;
    });

    // Scanner sensitivity control FIRST: a planted sentinel in a catalog
    // table must be found, or a broken scan would pass as compliance.
    client
        .batch_execute(&format!(
            "CREATE TABLE scanner_control (payload text); \
             INSERT INTO scanner_control VALUES ('{CONTROL_SENTINEL}');"
        ))
        .await
        .expect("plant control");
    assert!(
        !catalog_hits(&client, CONTROL_SENTINEL).await.is_empty(),
        "the scanner must be able to see a planted sentinel"
    );
    client
        .batch_execute("DROP TABLE scanner_control;")
        .await
        .expect("drop control");

    // THE INVARIANT: none of the customer sentinels appear in any
    // catalog table (outside the sanctioned CRDT pair).
    for needle in [PAGE_SENTINEL, CHAT_SENTINEL, EVENT_SENTINEL] {
        let hits = catalog_hits(&client, needle).await;
        assert!(
            hits.is_empty(),
            "customer bytes leaked into the catalog ({needle}):\n{}",
            hits.join("\n")
        );
    }

    // Positive control: the same bytes DO exist — as parquet on the
    // DATA_PATH — so their absence from the catalog is residency, not
    // absence of the data.
    for needle in [PAGE_SENTINEL, CHAT_SENTINEL, EVENT_SENTINEL] {
        assert!(
            !files_containing(lake_dir.path(), needle).is_empty(),
            "{needle} must be present in the lake's data files"
        );
    }
}
