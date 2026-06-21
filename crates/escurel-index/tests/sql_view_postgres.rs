//! Live SQL-view round-trip over a REAL Postgres (testcontainer) — the
//! connector the offline `json_dir`/`parquet_dir` tests can't exercise. Real
//! DuckDB `postgres_scanner` `ATTACH … (READ_ONLY)`, real libpq seeding, no
//! mocks. Requires Docker (like the MinIO S3 tests) + the DuckDB `postgres`
//! extension (auto-installed on first ATTACH).
//!
//! Closes the audit gap: postgres was previously only "loadability"-tested.
//! Here a real table round-trips through `create_instance` → `project_view`,
//! and the live source is mutated to prove `validate_bindings` fails closed on
//! schema drift and on a deleted credential.

use std::sync::Arc;

use duckdb::Connection;
use escurel_embed::{Embedder, ZeroEmbedder};
use escurel_index::backend::{SqlConnector, SqlViewBackend, SqlViewBinding};
use escurel_index::{Indexer, Migrator};
use escurel_storage::{FsStore, LaneStore};
use tempfile::TempDir;
use testcontainers_modules::postgres::Postgres;
use testcontainers_modules::testcontainers::runners::AsyncRunner;
use tokio_postgres::NoTls;

const TENANT: &str = "acme";

fn pg_binding(attach: &str) -> SqlViewBinding {
    SqlViewBinding {
        connector: SqlConnector::Postgres,
        attach: Some(attach.to_owned()),
        relation: "public.deals".to_owned(),
        filter: None,
        project: [("title".to_owned(), "title".to_owned())]
            .into_iter()
            .collect(),
        search_text: vec!["title".to_owned()],
    }
}

#[tokio::test]
async fn sql_view_over_live_postgres_round_trips_and_fails_closed_on_drift() {
    // 1. A real Postgres in a container.
    let pg = Postgres::default().start().await.expect("start postgres");
    let port = pg.get_host_port_ipv4(5432).await.expect("pg port");
    let dsn = format!("host=127.0.0.1 port={port} user=postgres password=postgres dbname=postgres");

    // 2. Seed a table + rows with a real libpq client.
    let (client, conn) = tokio_postgres::connect(&dsn, NoTls)
        .await
        .expect("pg connect");
    tokio::spawn(async move {
        let _ = conn.await;
    });
    client
        .batch_execute(
            "CREATE TABLE public.deals (id INT, title TEXT); \
             INSERT INTO public.deals VALUES \
               (1,'Acme widget'),(2,'Globex gadget'),(3,'Initech gizmo');",
        )
        .await
        .expect("seed deals");

    // 3. escurel indexer + the server-side credential (the DSN never touches
    //    markdown — REQ-SQL-05).
    let store_dir = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    let store: Arc<dyn LaneStore> = Arc::new(FsStore::new(store_dir.path().to_path_buf()));
    let embedder: Arc<dyn Embedder> = Arc::new(ZeroEmbedder::default());
    let dconn = Connection::open(db_dir.path().join("escurel.duckdb")).unwrap();
    Migrator::up(&dconn).unwrap();
    let indexer = Arc::new(Indexer::new(store, embedder, dconn, TENANT).unwrap());
    indexer
        .register_credential("crm_pg", "postgres", &dsn, Some("admin"))
        .await
        .unwrap();

    // 4. Materialise a read-only view over the live table.
    let m = SqlViewBackend::new(Arc::clone(&indexer))
        .create_instance("deals", &pg_binding("crm_pg"), "all", "# Deals")
        .await
        .expect("create_instance over live postgres");

    // 5. Project the rows back through DuckDB's postgres_scanner.
    let rows = indexer.project_view(&m.view, 10).await.expect("project");
    assert_eq!(rows.len(), 3, "all 3 live rows project: {rows:?}");
    let titles: Vec<&str> = rows
        .iter()
        .filter_map(|r| r.get("title").and_then(|v| v.as_str()))
        .collect();
    assert!(titles.contains(&"Acme widget"), "got {titles:?}");

    // 6. The healthy binding validates ok.
    let healthy = indexer.validate_bindings().await.unwrap();
    assert!(
        healthy.iter().any(|b| b.view == m.view && b.status == "ok"),
        "healthy binding must be ok: {healthy:?}"
    );

    // 7. Drift: mutate the source schema → validate fails closed.
    client
        .batch_execute("ALTER TABLE public.deals ADD COLUMN region TEXT;")
        .await
        .unwrap();
    let drifted = indexer.validate_bindings().await.unwrap();
    let st = drifted
        .iter()
        .find(|b| b.view == m.view)
        .expect("binding present");
    assert_eq!(
        st.status, "binding_degraded",
        "schema drift must degrade the binding: {drifted:?}"
    );

    // 8. Deleted credential: the source is unreachable → fail closed.
    indexer.delete_credential("crm_pg").await.unwrap();
    let gone = indexer.validate_bindings().await.unwrap();
    let st = gone
        .iter()
        .find(|b| b.view == m.view)
        .expect("binding present");
    assert_eq!(
        st.status, "backend_unavailable",
        "deleted credential must mark the binding unavailable: {gone:?}"
    );
}
