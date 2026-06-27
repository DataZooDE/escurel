//! End-to-end test for the `query_instance` agent tool over `POST /mcp`
//! (issue #205). Real gateway, real DuckDB, real `FsStore`, real reqwest. A
//! `sql_view` instance is materialised over an offline `json_dir`, a
//! `[[query::*]]` report page declaring `target:` is seeded, and the tool is
//! driven over the wire — proving the runtime params bind (an injection value
//! is inert) and the aggregation runs in the view.

use std::sync::Arc;

use duckdb::Connection;
use escurel_embed::{Embedder, ZeroEmbedder};
use escurel_index::backend::{SqlConnector, SqlViewBackend, SqlViewBinding};
use escurel_index::{Indexer, Migrator};
use escurel_test_support::{AuthMode, ConfigOverrides, EscurelProcess, Opts};
use serde_json::{Value, json};
use tempfile::TempDir;

const TENANT: &str = "acme";

const SKILL_CUSTOMERS: &str = "\
---
type: skill
id: customers
description: EU customers, mirrored read-only.
backend:
  kind: sql_view
  source: { connector: json_dir, relation: /unused }
  search_text: [name]
---
# customers
";

const SKILL_QUERY: &str = "\
---
type: skill
id: query
description: Reusable parameterised reads.
---
# query
";

/// Aggregating report over the customers view; `:min` is bound at call time
/// and `{{target}}` resolves to the managed view identifier.
const QUERY_BY_NAME: &str = "\
---
type: instance
skill: query
id: customers-by-name
target: \"[[customers::eu]]\"
params:
  - {name: min, type: number, required: true}
sql: \"SELECT name, SUM(amount)::BIGINT AS total FROM {{target}} WHERE amount >= :min GROUP BY name ORDER BY name\"
---
# customers-by-name
";

struct Setup {
    process: EscurelProcess,
    _dirs: Vec<TempDir>,
}

async fn setup() -> Setup {
    let store_dir = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    let data_dir = TempDir::new().unwrap();
    std::fs::write(
        data_dir.path().join("a.json"),
        br#"{"name":"Acme","amount":30}"#,
    )
    .unwrap();
    std::fs::write(
        data_dir.path().join("b.json"),
        br#"{"name":"Acme","amount":20}"#,
    )
    .unwrap();
    std::fs::write(
        data_dir.path().join("c.json"),
        br#"{"name":"Globex","amount":5}"#,
    )
    .unwrap();

    let store = Arc::new(escurel_storage::FsStore::new(
        store_dir.path().to_path_buf(),
    ));
    let embedder: Arc<dyn Embedder> = Arc::new(ZeroEmbedder::default());
    let conn = Connection::open(db_dir.path().join("escurel.duckdb")).unwrap();
    Migrator::up(&conn).unwrap();
    let indexer = Arc::new(Indexer::new(store, embedder, conn, TENANT).unwrap());

    indexer
        .update_page("markdown/skills/customers.md", SKILL_CUSTOMERS)
        .await
        .unwrap();
    indexer
        .update_page("markdown/skills/query.md", SKILL_QUERY)
        .await
        .unwrap();
    let binding = SqlViewBinding {
        connector: SqlConnector::JsonDir,
        attach: None,
        relation: data_dir.path().to_str().unwrap().to_owned(),
        filter: None,
        project: Default::default(),
        search_text: vec!["name".to_owned()],
    };
    SqlViewBackend::new(Arc::clone(&indexer))
        .create_instance("customers", &binding, "eu", "# EU customers")
        .await
        .unwrap();
    indexer
        .update_page(
            "markdown/instances/query/customers-by-name.md",
            QUERY_BY_NAME,
        )
        .await
        .unwrap();

    let process = EscurelProcess::spawn(Opts {
        auth: AuthMode::Disabled,
        config_overrides: ConfigOverrides {
            indexer: Some(indexer),
            ..Default::default()
        },
        ..Default::default()
    })
    .await;

    Setup {
        process,
        _dirs: vec![store_dir, db_dir, data_dir],
    }
}

async fn call(p: &EscurelProcess, name: &str, args: Value) -> Value {
    reqwest::Client::new()
        .post(p.mcp_url())
        .json(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": { "name": name, "arguments": args },
        }))
        .send()
        .await
        .expect("post")
        .json()
        .await
        .expect("json")
}

#[tokio::test]
async fn query_instance_aggregates_and_binds_over_the_wire() {
    let s = setup().await;

    // Happy path: `:min` = 10 drops Globex (amount 5); Acme aggregates to 50.
    // `ref` is the documented wire key; the `[[query::id]]` wikilink form is
    // accepted and normalised server-side.
    let body = call(
        &s.process,
        "query_instance",
        json!({ "ref": "[[query::customers-by-name]]", "params": { "min": 10 } }),
    )
    .await;
    assert!(body.get("error").is_none(), "unexpected error: {body}");
    let r = &body["result"]["structuredContent"];
    let rows = r["rows"].as_array().expect("rows array");
    assert_eq!(rows.len(), 1, "Globex filtered out: {rows:?}");
    assert_eq!(rows[0]["name"], "Acme");
    assert_eq!(rows[0]["total"].as_i64().unwrap(), 50);
    assert_eq!(r["truncated"], false);
    assert!(
        r["schema"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["name"] == "total"),
        "schema names the aggregate column: {}",
        r["schema"]
    );

    // Injection-as-a-value: a runtime value is bound, never interpolated. A
    // huge `:min` simply matches no rows; `pages` is untouched.
    let body = call(
        &s.process,
        "query_instance",
        json!({ "ref": "customers-by-name", "params": { "min": 999999 } }),
    )
    .await;
    let rows = body["result"]["structuredContent"]["rows"]
        .as_array()
        .expect("rows array");
    assert!(rows.is_empty(), "no customer clears the floor: {rows:?}");

    s.process.shutdown().await;
}
