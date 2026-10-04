//! The agent-facing quality of the MCP surface (AX review, 2026-10-04): what an LLM agent sees when it
//! calls the tools. Every test talks JSON-RPC over real HTTP to a real gateway (real DuckDB +
//! `FsStore`, rows read from real JSON files); nothing is stubbed.
//!
//! The reviewer's failing cases, one test each: a declared-filterable column is rejected, `limit`
//! bounds are advisory only, cursors are guessable hex, domain errors on read tools are bare
//! JSON-RPC strings, `capture_event` accepts a label no skill answers to, and so on.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use duckdb::Connection;
use escurel_embed::{Embedder, ZeroEmbedder};
use escurel_index::{Indexer, Migrator};
use escurel_storage::{FsStore, LaneStore};
use escurel_test_support::{AuthMode, ConfigOverrides, EscurelProcess, Opts};
use serde_json::{Value, json};
use tempfile::TempDir;

const TENANT: &str = "acme";
const SKILL: &str = "markdown/skills/sales-order.md";
const ROWS: usize = 60;

/// The id of generated row `n` (a SAP-style zero-padded document number).
fn doc(n: usize) -> String {
    format!("{:010}", 4_500_000 + n)
}
fn row_page(n: usize) -> String {
    format!("markdown/instances/sales-order/{}.md", doc(n))
}

/// Write `ROWS` rows as five JSON files of an array each (the shape `read_json_auto` infers a typed
/// schema from: VARCHAR key, DOUBLE value, DATE).
fn write_source(dir: &Path, skip: Option<usize>) {
    std::fs::create_dir_all(dir).unwrap();
    for file in 0..2 {
        let mut rows = Vec::new();
        for n in file * 30..(file + 1) * 30 {
            if Some(n) == skip {
                continue;
            }
            rows.push(json!({
                "vbeln": doc(n),
                "kunnr": format!("{:07}", 1_000_000 + n % 40),
                "netwr": 100.0 + n as f64,
                "erdat": format!("2026-09-{:02}", 1 + n % 28),
                "internal_note": "never on the wire",
            }));
        }
        std::fs::write(
            dir.join(format!("vbak-{file}.json")),
            serde_json::to_string(&rows).unwrap(),
        )
        .unwrap();
    }
}

fn skill_page(src: &Path) -> String {
    format!(
        r#"---
kind: skill
id: sales-order
description: A sales order, one instance per row of the SAP extract.
fields:
  - {{name: sales_doc, kind: string, required: true, label: "Sales document (VBELN)"}}
  - {{name: sold_to, kind: string, label: "Sold-to party (KUNNR)"}}
  - {{name: net_value, kind: float, label: "Net value (NETWR)", render: money}}
  - {{name: created, kind: date, label: "Created (ERDAT)", render: date}}
  - {{name: delivery_risk, kind: enum, values: [low, medium, high], label: "Delivery risk"}}
backend:
  kind: sql_view
  instances: rows
  key: vbeln
  linked: markdown
  filterable: [kunnr]
  source: {{connector: json_dir, relation: "{}"}}
  project: {{vbeln: sales_doc, kunnr: sold_to, netwr: net_value, erdat: created}}
---
# sales-order
"#,
        src.display()
    )
}

/// The whole JSON-RPC response (`result` with `isError`, or `error`), for tests that care which of
/// the two an agent gets.
async fn rpc(p: &EscurelProcess, name: &str, args: Value) -> Value {
    reqwest::Client::new()
        .post(p.mcp_url())
        .json(&json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                       "params": { "name": name, "arguments": args } }))
        .send()
        .await
        .expect("post")
        .json()
        .await
        .expect("json")
}

async fn call_raw(p: &EscurelProcess, name: &str, args: Value) -> Value {
    let body = rpc(p, name, args).await;
    assert!(body.get("error").is_none(), "{name}: {body}");
    body["result"]["structuredContent"].clone()
}

/// A refusal an agent can act on: `isError: true` and an `issues[]` entry with `code`.
fn refusal_issue(body: &Value) -> Value {
    assert!(
        body.get("error").is_none(),
        "a domain error must not be a bare JSON-RPC error: {body}"
    );
    assert_eq!(body["result"]["isError"], true, "{body}");
    let issues = body["result"]["structuredContent"]["issues"]
        .as_array()
        .unwrap_or_else(|| panic!("issues[] expected: {body}"));
    issues[0].clone()
}

struct Rows {
    p: EscurelProcess,
    _dirs: Vec<TempDir>,
}

impl Rows {
    async fn start() -> Self {
        let store_dir = TempDir::new().unwrap();
        let db_dir = TempDir::new().unwrap();
        let src_dir = TempDir::new().unwrap();
        let src = src_dir.path().join("vbak");
        write_source(&src, None);
        let store: Arc<dyn LaneStore> = Arc::new(FsStore::new(store_dir.path().to_path_buf()));
        let embedder: Arc<dyn Embedder> = Arc::new(ZeroEmbedder::default());
        let conn = Connection::open(db_dir.path().join("escurel.duckdb")).unwrap();
        Migrator::up(&conn).unwrap();
        let indexer = Arc::new(Indexer::new(store, embedder, conn, TENANT).unwrap());
        let p = EscurelProcess::spawn(Opts {
            auth: AuthMode::Disabled,
            config_overrides: ConfigOverrides {
                indexer: Some(indexer),
                ..Default::default()
            },
            ..Default::default()
        })
        .await;
        let r = call_raw(
            &p,
            "update_page",
            json!({ "page_id": SKILL, "content": skill_page(&src) }),
        )
        .await;
        assert_eq!(r["ok"], true, "the skill page is accepted: {r}");
        Self {
            p,
            _dirs: vec![store_dir, db_dir, src_dir],
        }
    }

    async fn call(&self, name: &str, args: Value) -> Value {
        call_raw(&self.p, name, args).await
    }
    async fn rpc(&self, name: &str, args: Value) -> Value {
        rpc(&self.p, name, args).await
    }
}

// ---- (1) filter names -------------------------------------------------------------------------

#[tokio::test]
async fn a_declared_filterable_column_is_accepted_by_its_column_name_and_by_its_frontmatter_alias()
{
    let t = Rows::start().await;
    // `kunnr` is what the skill DECLARES filterable; `sold_to` is the field name an agent sees on the
    // page. Both must work, with the same result.
    let by_column = t
        .call(
            "list_instances",
            json!({ "skill_id": "sales-order", "frontmatter_key": "kunnr", "frontmatter_value": "1000007" }),
        )
        .await;
    let by_alias = t
        .call(
            "list_instances",
            json!({ "skill_id": "sales-order", "frontmatter_key": "sold_to", "frontmatter_value": "1000007" }),
        )
        .await;
    let n = by_alias["instances"].as_array().unwrap().len();
    assert!(
        n > 0,
        "the generated data has rows for that customer: {by_alias}"
    );
    assert_eq!(by_column["instances"].as_array().unwrap().len(), n);
}

#[tokio::test]
async fn a_field_that_is_not_filterable_says_which_ones_are_and_where_free_text_goes() {
    let t = Rows::start().await;
    let body = t
        .rpc(
            "list_instances",
            json!({ "skill_id": "sales-order", "frontmatter_key": "net_value", "frontmatter_value": "1" }),
        )
        .await;
    let issue = refusal_issue(&body);
    assert_eq!(issue["code"], "field_not_filterable", "{issue}");
    let msg = issue["message"].as_str().unwrap();
    assert!(
        msg.contains("sold_to") && msg.contains("kunnr"),
        "valid names listed: {msg}"
    );
    assert!(msg.contains("search"), "free text is `search`: {msg}");
}

// ---- (2) limit bounds and cursors -------------------------------------------------------------

#[tokio::test]
async fn limit_outside_its_documented_range_is_refused_with_the_range() {
    let t = Rows::start().await;
    for bad in [json!(0), json!(10_001), json!("x"), json!(-3)] {
        let body = t
            .rpc(
                "list_instances",
                json!({ "skill_id": "sales-order", "limit": bad }),
            )
            .await;
        let issue = refusal_issue(&body);
        assert_eq!(issue["code"], "invalid_limit", "limit {bad}: {issue}");
        let msg = issue["message"].as_str().unwrap();
        assert!(
            msg.contains("1") && msg.contains("10000"),
            "range in the message: {msg}"
        );
        assert!(!msg.contains("usize"), "no Rust type names: {msg}");
    }
    // Every other list tool that declares a limit is held to the same bounds.
    let body = t.rpc("list_inbox", json!({ "limit": 0 })).await;
    assert_eq!(refusal_issue(&body)["code"], "invalid_limit");
}

#[tokio::test]
async fn a_cursor_is_opaque_and_a_forged_one_says_how_to_recover() {
    let t = Rows::start().await;
    let page = t
        .call(
            "list_instances",
            json!({ "skill_id": "sales-order", "limit": 3 }),
        )
        .await;
    let cursor = page["next_cursor"]
        .as_str()
        .expect("more rows follow")
        .to_owned();
    // It used to be the plain hex of the last row's key (guessable, forgeable by anyone who could
    // count). A key like 4500002 must not be readable from it.
    let hex_of_key = "30303034353030303032"; // "0004500002"
    assert!(
        !cursor.contains(hex_of_key),
        "the cursor is an envelope, not the key: {cursor}"
    );
    // A real cursor still resumes.
    let next = t
        .call(
            "list_instances",
            json!({ "skill_id": "sales-order", "limit": 3, "cursor": cursor }),
        )
        .await;
    assert_eq!(next["instances"].as_array().unwrap().len(), 3);
    // Forged, truncated and tampered cursors get one worded, actionable refusal.
    for forged in ["zzz", "aéb", "", &cursor[..cursor.len() - 3]] {
        let body = t
            .rpc(
                "list_instances",
                json!({ "skill_id": "sales-order", "limit": 3, "cursor": forged }),
            )
            .await;
        let issue = refusal_issue(&body);
        assert_eq!(
            issue["code"], "invalid_cursor",
            "cursor {forged:?}: {issue}"
        );
        assert!(
            issue["message"]
                .as_str()
                .unwrap()
                .contains("restart without `cursor`"),
            "{issue}"
        );
    }
}
