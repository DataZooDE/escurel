//! Writer lease over a REAL Postgres (testcontainer) — the guard that
//! makes `ESCUREL_ROLE=writer` single-instance against a shared DuckLake
//! catalog (#371).
//!
//! Two replicas both booting as writer against one catalog lose
//! acknowledged writes: each publishes its own snapshot of the whole
//! lake and prunes parquet the other just committed (measured 17/40
//! survivors in the issue's reproducer). The lease is a Postgres
//! advisory lock held on a dedicated session for the writer's lifetime:
//! second writer → refused at boot; holder dies → lock auto-releases
//! with its session and a successor acquires.
//!
//! Opt-in: gated behind the `live-postgres` feature (needs Docker),
//! mirroring `sql_view_postgres.rs`. Run with
//! `cargo test -p escurel-index --features live-postgres --test suite writer_lease`.
#![cfg(feature = "live-postgres")]

use escurel_index::snapshot::WriterLease;
use testcontainers_modules::postgres::Postgres;
use testcontainers_modules::testcontainers::runners::AsyncRunner;

#[tokio::test]
async fn second_writer_is_refused_until_the_first_releases() {
    let pg = Postgres::default().start().await.expect("start postgres");
    let port = pg.get_host_port_ipv4(5432).await.expect("pg port");
    let dsn = format!("host=127.0.0.1 port={port} user=postgres password=postgres dbname=postgres");

    // First writer takes the lease.
    let first = WriterLease::acquire(&dsn)
        .await
        .expect("catalog reachable")
        .expect("first writer acquires");

    // A second writer against the SAME catalog must be refused — this is
    // the exact `replicaCount: 2` misconfiguration from #371, caught at
    // boot instead of surfacing as silently pruned parquet.
    let second = WriterLease::acquire(&dsn).await.expect("catalog reachable");
    assert!(
        second.is_none(),
        "two writers must never hold the lease at once"
    );

    // Releasing (dropping) the first frees the lock with its session, so
    // a successor — the STOP-FIRST redeploy case — acquires cleanly.
    drop(first);
    let successor = WriterLease::acquire(&dsn).await.expect("catalog reachable");
    assert!(
        successor.is_some(),
        "a successor acquires after the holder releases"
    );
}

#[tokio::test]
async fn lease_is_scoped_per_database() {
    let pg = Postgres::default().start().await.expect("start postgres");
    let port = pg.get_host_port_ipv4(5432).await.expect("pg port");
    let dsn_a =
        format!("host=127.0.0.1 port={port} user=postgres password=postgres dbname=postgres");

    // A second CATALOG (another database on the same instance — the
    // prod/nonprod-share-a-server shape) must not be blocked by the
    // first catalog's writer: advisory locks are per-database.
    let (client, conn) = tokio_postgres::connect(&dsn_a, tokio_postgres::NoTls)
        .await
        .expect("pg connect");
    tokio::spawn(async move {
        let _ = conn.await;
    });
    client
        .batch_execute("CREATE DATABASE other_catalog;")
        .await
        .expect("create second database");
    let dsn_b =
        format!("host=127.0.0.1 port={port} user=postgres password=postgres dbname=other_catalog");

    let _writer_a = WriterLease::acquire(&dsn_a)
        .await
        .expect("catalog reachable")
        .expect("writer A acquires");
    let writer_b = WriterLease::acquire(&dsn_b)
        .await
        .expect("catalog reachable");
    assert!(
        writer_b.is_some(),
        "a different catalog database is a different lease domain"
    );
}
