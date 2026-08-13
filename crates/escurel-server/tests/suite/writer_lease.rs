//! Two ducklake WRITERS against one shared Postgres catalog, LIVE
//! (#371): the second must be refused at boot.
//!
//! The reproducer behind this: 2 replicas with `ESCUREL_ROLE=writer`
//! against one catalog acknowledged all 40 concurrent writes, then the
//! readable page count fell to 17/40 — each writer published its own
//! snapshot of the whole lake and pruned parquet the other had just
//! committed. Nothing in the config surface flagged the topology; the
//! failure was silent. The single-writer lease (a Postgres advisory
//! lock on the catalog, `escurel_index::snapshot::WriterLease`) turns
//! it into a loud boot error, and releases with the holder's session
//! so a STOP-FIRST redeploy hands over cleanly.
//!
//! Opt-in: gated behind the `live-ducklake` feature (needs Docker). Run
//! with `cargo test -p escurel-server --features live-ducklake --test
//! suite writer_lease`.

#![cfg(feature = "live-ducklake")]

use std::collections::HashMap;

use escurel_server::EscurelConfig;
use tempfile::TempDir;
use testcontainers_modules::postgres::Postgres;
use testcontainers_modules::testcontainers::runners::AsyncRunner;

fn env_map(pairs: Vec<(&str, String)>) -> HashMap<String, String> {
    pairs.into_iter().map(|(k, v)| (k.to_owned(), v)).collect()
}

fn source(map: HashMap<String, String>) -> impl Fn(&str) -> Option<String> {
    move |k: &str| map.get(k).cloned()
}

fn writer_cfg(
    data_dir: &TempDir,
    lake_data: &str,
    dsn: &str,
    extra: &[(&str, &str)],
) -> EscurelConfig {
    let mut pairs = vec![
        (
            "ESCUREL_SERVER_DATA_DIR",
            data_dir.path().to_str().unwrap().to_owned(),
        ),
        ("ESCUREL_SERVER_LISTEN_HTTP", "127.0.0.1:0".to_owned()),
        (
            "ESCUREL_OBSERVABILITY_METRICS_LISTEN",
            "127.0.0.1:0".to_owned(),
        ),
        ("ESCUREL_TENANT", "acme".to_owned()),
        ("ESCUREL_EMBEDDING_PROVIDER", "zero".to_owned()),
        ("ESCUREL_INDEX_BACKEND", "ducklake".to_owned()),
        ("ESCUREL_ROLE", "writer".to_owned()),
        ("ESCUREL_DUCKLAKE_CATALOG_DSN", dsn.to_owned()),
        ("ESCUREL_DUCKLAKE_DATA_PATH", lake_data.to_owned()),
    ];
    for (k, v) in extra {
        pairs.push((k, (*v).to_owned()));
    }
    EscurelConfig::from_source(&source(env_map(pairs))).expect("writer config parses")
}

#[tokio::test]
async fn second_writer_against_the_same_catalog_fails_boot() {
    let pg = Postgres::default().start().await.expect("start postgres");
    let port = pg.get_host_port_ipv4(5432).await.expect("pg port");
    let dsn = format!("host=127.0.0.1 port={port} user=postgres password=postgres dbname=postgres");

    let lake_dir = TempDir::new().unwrap();
    std::fs::create_dir_all(lake_dir.path().join("data")).unwrap();
    let lake_data = lake_dir.path().join("data").to_str().unwrap().to_owned();

    // Writer 1 boots and holds the lease; scoped so its drop below is
    // the STOP-FIRST "old writer stops" moment.
    {
        let data1 = TempDir::new().unwrap();
        let first = writer_cfg(&data1, &lake_data, &dsn, &[])
            .build()
            .await
            .expect("first writer boots");

        // Writer 2 — the `replicaCount: 2` misconfiguration — must FAIL,
        // loudly, at boot, instead of acknowledging writes it will lose.
        let data2 = TempDir::new().unwrap();
        let second = writer_cfg(&data2, &lake_data, &dsn, &[]).build().await;
        let err = match second {
            Err(e) => format!("{e}"),
            Ok(_) => panic!("a second writer must not boot against a held lease"),
        };
        assert!(
            err.contains("writer"),
            "the refusal names the single-writer contract: {err}"
        );

        // Explicit opt-out keeps an operator with an exotic catalog in
        // business — their risk, their flag.
        let data3 = TempDir::new().unwrap();
        let opted_out = writer_cfg(&data3, &lake_data, &dsn, &[("ESCUREL_WRITER_LEASE", "off")])
            .build()
            .await
            .expect("lease=off skips the guard");
        opted_out.handle.shutdown().await;

        first.handle.shutdown().await;
        // `first`'s remaining fields (the lease among them) drop here —
        // the catalog session ends, releasing the advisory lock.
    }
    let data4 = TempDir::new().unwrap();
    let successor = writer_cfg(&data4, &lake_data, &dsn, &[])
        .build()
        .await
        .expect("successor boots after the holder released");
    successor.handle.shutdown().await;
}
