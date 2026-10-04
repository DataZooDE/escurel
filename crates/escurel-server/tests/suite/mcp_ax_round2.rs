//! Round-2 agent-usability contract (second crew review of the OKF branch), over real HTTP:
//! unknown arguments are refused, drafts carry `write_back` in their schema, empty successes are
//! errors, cursors are signed. One real gateway, real bearers; no mocks.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use duckdb::Connection;
use escurel_embed::{Embedder, ZeroEmbedder};
use escurel_index::{Indexer, Migrator};
use escurel_storage::{FsStore, LaneStore};
use escurel_test_support::{AuthMode, ConfigOverrides, EscurelProcess, FixtureBuilder, Opts, Role};
use serde_json::{Value, json};
use tempfile::TempDir;

const TENANT: &str = "stuttgart-ai";
const NOTE_SKILL: &str = "---\nkind: skill\nid: note\ndescription: A note.\n\
    visibility: public\n---\n# note\n";
const NOTE_A: &str = "---\nkind: instance\nskill: note\nid: a\n---\n# A\n";

async fn start() -> EscurelProcess {
    EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        config_overrides: ConfigOverrides::default(),
        fixtures: Some(
            FixtureBuilder::new()
                .tenant(TENANT)
                .skill("note", NOTE_SKILL)
                .instance("note", "a", NOTE_A)
                .done(),
        ),
    })
    .await
}

async fn call(p: &EscurelProcess, token: &str, name: &str, args: Value) -> Value {
    reqwest::Client::new()
        .post(p.mcp_url())
        .header("authorization", format!("Bearer {token}"))
        .json(&json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": { "name": name, "arguments": args },
        }))
        .send()
        .await
        .expect("post")
        .json()
        .await
        .expect("decode")
}

fn first_issue(v: &Value) -> Value {
    v["result"]["structuredContent"]["issues"][0].clone()
}

/// `filter`/`limt` used to be dropped silently, so the call "succeeded" with default behaviour.
#[tokio::test]
async fn unknown_arguments_are_refused_with_a_did_you_mean() {
    let p = start().await;
    let t = p.mint_token(TENANT, Role::Agent);

    let r = call(
        &p,
        &t,
        "list_instances",
        json!({ "skill_id": "note", "limt": 5 }),
    )
    .await;
    assert_eq!(r["result"]["isError"], json!(true), "{r}");
    let i = first_issue(&r);
    assert_eq!(i["code"], "invalid_argument", "{r}");
    let msg = i["message"].as_str().unwrap();
    assert!(
        msg.contains("limt") && msg.contains("limit"),
        "did-you-mean: {msg}"
    );
    assert!(
        msg.contains("skill_id"),
        "lists the valid parameters: {msg}"
    );

    // The documented aliases and the plain call keep working.
    let ok = call(&p, &t, "list_instances", json!({ "skill": "note" })).await;
    assert_eq!(ok["result"]["isError"], json!(false), "{ok}");
    let ok = call(&p, &t, "list_skills", json!({})).await;
    assert_eq!(ok["result"]["isError"], json!(false), "{ok}");
    let bad = call(&p, &t, "list_skills", json!({ "filter": "x" })).await;
    assert_eq!(first_issue(&bad)["code"], "invalid_argument", "{bad}");
}

// ------------------------------------------------------------------ rows fixture ---

/// A rows skill over a real JSON source: `kunnr` is the sold-to code (filterable), `name1` the
/// customer's display name (searchable), `status` an enum the skill declares writable.
struct Rows {
    p: EscurelProcess,
    _dirs: Vec<TempDir>,
}

const ORDERS: usize = 25;

fn write_source(dir: &Path) {
    std::fs::create_dir_all(dir).unwrap();
    let rows: Vec<Value> = (0..ORDERS)
        .map(|n| {
            json!({
                "vbeln": format!("SO-{n:04}"),
                "kunnr": format!("{:07}", 1_000_000 + n % 5),
                "name1": if n == 7 { "Zwiebelmuster Handels GmbH".to_owned() } else { format!("Kunde {}", n % 5) },
                "status": "open",
                "netwr": 100.0 + n as f64,
            })
        })
        .collect();
    std::fs::write(
        dir.join("orders.json"),
        serde_json::to_string(&rows).unwrap(),
    )
    .unwrap();
}

fn orders_skill(src: &Path) -> String {
    format!(
        r#"---
kind: skill
id: sales-order
description: A sales order, one instance per row.
fields:
  - {{name: sales_doc, kind: string, required: true}}
  - {{name: sold_to, kind: string}}
  - {{name: customer, kind: string}}
  - {{name: status, kind: enum, values: [open, confirmed, moved]}}
backend:
  kind: sql_view
  instances: rows
  key: vbeln
  linked: markdown
  filterable: [kunnr]
  searchable: [name1]
  writable_columns: [status]
  source: {{connector: json_dir, relation: "{}"}}
  project: {{vbeln: sales_doc, kunnr: sold_to, name1: customer, status: status}}
---
# sales-order
"#,
        src.display()
    )
}

impl Rows {
    async fn start() -> Self {
        let store_dir = TempDir::new().unwrap();
        let db_dir = TempDir::new().unwrap();
        let src_dir = TempDir::new().unwrap();
        let src: PathBuf = src_dir.path().join("orders");
        write_source(&src);
        let store: Arc<dyn LaneStore> = Arc::new(FsStore::new(store_dir.path().to_path_buf()));
        let embedder: Arc<dyn Embedder> = Arc::new(ZeroEmbedder::default());
        let conn = Connection::open(db_dir.path().join("escurel.duckdb")).unwrap();
        Migrator::up(&conn).unwrap();
        let indexer = Arc::new(Indexer::new(store, embedder, conn, "acme").unwrap());
        let p = EscurelProcess::spawn(Opts {
            auth: AuthMode::Disabled,
            config_overrides: ConfigOverrides {
                indexer: Some(indexer),
                ..Default::default()
            },
            ..Default::default()
        })
        .await;
        let r = rows_call(
            &p,
            "update_page",
            json!({ "page_id": "markdown/skills/sales-order.md", "content": orders_skill(&src) }),
        )
        .await;
        assert_eq!(r["ok"], true, "the skill page is accepted: {r}");
        Self {
            p,
            _dirs: vec![store_dir, db_dir, src_dir],
        }
    }

    async fn call(&self, name: &str, args: Value) -> Value {
        rows_call(&self.p, name, args).await
    }
}

/// The full JSON-RPC answer (no bearer: the gateway runs without auth).
async fn rows_call(p: &EscurelProcess, name: &str, args: Value) -> Value {
    let body: Value = reqwest::Client::new()
        .post(p.mcp_url())
        .json(&json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                       "params": { "name": name, "arguments": args } }))
        .send()
        .await
        .expect("post")
        .json()
        .await
        .expect("json");
    assert!(body.get("error").is_none(), "{name}: {body}");
    body["result"]["structuredContent"].clone()
}

/// `list_skills` says what an agent needs to drive a rows skill: its key, what it can filter and
/// search by, and what it may write back — each column under the field name the rows show.
#[tokio::test]
async fn list_skills_describes_a_rows_backend() {
    let t = Rows::start().await;
    let r = t.call("list_skills", json!({})).await;
    let skill = r["skills"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["id"] == "sales-order")
        .cloned()
        .expect("sales-order listed");
    let b = &skill["backend"];
    assert_eq!(b["kind"], "sql_view", "{b}");
    assert_eq!(b["instances"], "rows", "{b}");
    assert_eq!(b["key"], json!(["vbeln"]), "{b}");
    assert_eq!(
        b["filterable"],
        json!([{"field": "sold_to", "column": "kunnr"}]),
        "{b}"
    );
    assert_eq!(
        b["searchable"],
        json!([{"field": "customer", "column": "name1"}]),
        "{b}"
    );
    assert_eq!(
        b["writable_columns"],
        json!([{"field": "status", "column": "status"}]),
        "{b}"
    );
    assert_eq!(b["writable_via"], "write_back", "{b}");
    assert_eq!(b["linked"], true, "{b}");
}

/// A customer is found by the name a person types, not only by its key: the skill declares the
/// display column `searchable`. A column it did NOT declare (`status`) still matches nothing.
#[tokio::test]
async fn a_row_is_findable_by_a_searchable_display_column() {
    let t = Rows::start().await;
    let r = t
        .call(
            "search",
            json!({ "q": "Zwiebelmuster", "skill": "sales-order" }),
        )
        .await;
    let hits = r["hits"].as_array().cloned().unwrap_or_default();
    assert_eq!(hits.len(), 1, "the one row whose customer matches: {r}");
    assert_eq!(
        hits[0]["page_id"], "markdown/instances/sales-order/SO-0007.md",
        "{r}"
    );
    assert!(
        hits[0]["snippet"]
            .as_str()
            .unwrap_or_default()
            .contains("customer"),
        "the snippet shows what matched under its field name: {r}"
    );
    assert!(
        hits[0].get("similarity").is_none(),
        "no sentinel similarity: {r}"
    );
    // `confirmed` is nowhere in the data and `open` is a column nobody declared searchable.
    let none = t
        .call("search", json!({ "q": "open", "skill": "sales-order" }))
        .await;
    assert!(
        none["hits"].as_array().unwrap().is_empty(),
        "undeclared column: {none}"
    );
}
