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
        config_overrides: ConfigOverrides {
            signing: true,
            ..Default::default()
        },
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

// ------------------------------------------------------- create_draft + write_back ---

use super::sql_rows_db::{Gw, db_row, doc, row_page, skill_page};

async fn etag_of(g: &Gw, n: usize) -> String {
    let r = g.admin("expand", json!({ "page_id": row_page(n) })).await;
    r["backend_projection"]["etag"].as_str().unwrap().to_owned()
}

/// `write_back` is a declared PARAMETER of `create_draft` (it used to be accepted only inside the
/// markdown frontmatter, and as an argument it was silently dropped).
#[tokio::test]
async fn create_draft_takes_write_back_as_an_argument() {
    let g = Gw::start().await;
    let tools = g.admin("list_skills", json!({})).await; // warm
    let _ = tools;
    let listed: Value = reqwest::Client::new()
        .post(g.p.mcp_url())
        .header(
            "authorization",
            format!("Bearer {}", g.p.mint_token("acme", Role::Admin)),
        )
        .json(&json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let cd = listed["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == "create_draft")
        .cloned()
        .unwrap();
    assert!(
        cd["inputSchema"]["properties"]["write_back"].is_object(),
        "{cd}"
    );
    let d = cd["description"].as_str().unwrap();
    assert!(
        d.contains("write_back") && d.contains("only a human"),
        "teaches it: {d}"
    );

    let e = etag_of(&g, 3).await;
    let r = g
        .admin(
            "create_draft",
            json!({
                "target_page_id": row_page(3),
                "write_back": { "patch": { "status": "shipped" }, "base_etag": e },
            }),
        )
        .await;
    assert_eq!(r["ok"], true, "no markdown needed: {r}");
    let id = r["draft"]["draft_id"].as_str().unwrap().to_owned();
    assert_eq!(
        db_row(&g.db, 3)["status"],
        "open",
        "a draft touches nothing"
    );
    let done = g.admin("promote_draft", json!({ "draft_id": id })).await;
    assert_eq!(done["ok"], true, "{done}");
    assert_eq!(
        db_row(&g.db, 3)["status"],
        "shipped",
        "the database applied it"
    );
    let _ = doc(3);
}

/// A value the skill's field cannot hold is refused when the draft is MADE, and costs nothing: no
/// dead draft is left to block the next proposal on that page.
#[tokio::test]
async fn a_write_back_value_outside_the_field_is_refused_at_draft_time() {
    let g = Gw::start().await;
    let enum_skill = skill_page().replace(
        "{name: status, kind: string, label: \"Status\"}",
        "{name: status, kind: enum, values: [open, shipped, moved], label: \"Status\"}",
    );
    let r = g
        .admin(
            "update_page",
            json!({ "page_id": "markdown/skills/shop-order.md", "content": enum_skill }),
        )
        .await;
    assert_eq!(r["ok"], true, "{r}");
    let e = etag_of(&g, 4).await;
    let bad = g
        .admin(
            "create_draft",
            json!({ "target_page_id": row_page(4),
                    "write_back": { "patch": { "status": "bogus" }, "base_etag": e } }),
        )
        .await;
    assert_eq!(bad["ok"], false, "{bad}");
    assert_eq!(
        bad["issues"][0]["code"], "write_back_invalid_value",
        "{bad}"
    );
    let msg = bad["issues"][0]["message"].as_str().unwrap();
    assert!(
        msg.contains("status must be one of open|shipped|moved") && msg.contains("bogus"),
        "{msg}"
    );
    let bad_int = g
        .admin(
            "create_draft",
            json!({ "target_page_id": row_page(4),
                    "write_back": { "patch": { "quantity": "many" }, "base_etag": e } }),
        )
        .await;
    assert_eq!(
        bad_int["issues"][0]["code"], "write_back_invalid_value",
        "{bad_int}"
    );

    // Nothing was left behind: the next, valid proposal is not blocked by a dead draft.
    let ok = g
        .admin(
            "create_draft",
            json!({ "target_page_id": row_page(4),
                    "write_back": { "patch": { "status": "moved" }, "base_etag": e } }),
        )
        .await;
    assert_eq!(ok["ok"], true, "{ok}");

    // A second open draft on the page conflicts, and the refusal says how to get out.
    let again = g
        .admin(
            "create_draft",
            json!({ "target_page_id": row_page(4),
                    "write_back": { "patch": { "status": "shipped" }, "base_etag": e } }),
        )
        .await;
    assert_eq!(again["issues"][0]["code"], "conflict", "{again}");
    assert!(
        again["issues"][0]["suggestion"]
            .as_str()
            .unwrap_or_default()
            .contains("discard_draft"),
        "{again}"
    );
}

// ------------------------------------------------------------------ empty successes ---

fn text_of(v: &Value) -> String {
    v["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_default()
        .to_owned()
}

/// `list_instances` of a skill that does not exist used to answer an empty success; it is the
/// caller's mistake, and the answer names the skills there are.
#[tokio::test]
async fn list_instances_of_an_unknown_skill_is_an_error_naming_the_known_ones() {
    let p = start().await;
    let t = p.mint_token(TENANT, Role::Agent);
    let r = call(&p, &t, "list_instances", json!({ "skill_id": "nope" })).await;
    assert_eq!(r["result"]["isError"], json!(true), "{r}");
    let i = first_issue(&r);
    assert_eq!(i["code"], "unknown_skill", "{r}");
    assert!(
        i["message"].as_str().unwrap().contains("note"),
        "names the known skills: {r}"
    );
    // A known skill with no instances is still an honest empty list.
    let ok = call(&p, &t, "list_instances", json!({ "skill_id": "note" })).await;
    assert_eq!(ok["result"]["isError"], json!(false), "{ok}");
}

/// A text-only client sees what a JSON client does: not-found says so, a page says where the next
/// one is, a refusal keeps its guidance whole, and a minted token is announced without being
/// repeated into a transcript.
#[tokio::test]
async fn the_summary_text_carries_the_control_data() {
    let p = start().await;
    let t = p.mint_token(TENANT, Role::Agent);

    let missing = call(
        &p,
        &t,
        "expand",
        json!({ "page_id": "markdown/instances/note/ghost.md" }),
    )
    .await;
    let text = text_of(&missing);
    assert!(
        text.to_lowercase().contains("not found") && text.contains("page: null"),
        "{text}"
    );
    assert!(!text.contains("2 keys"), "{text}");

    // Two pages of one note each: the summary names the cursor.
    let admin = p.mint_token(TENANT, Role::Admin);
    let b = "---\nkind: instance\nskill: note\nid: b\n---\n# B\n";
    let w = call(
        &p,
        &admin,
        "update_page",
        json!({ "page_id": "markdown/instances/note/b.md", "content": b }),
    )
    .await;
    assert_eq!(w["result"]["structuredContent"]["ok"], true, "{w}");
    let page = call(
        &p,
        &t,
        "list_instances",
        json!({ "skill_id": "note", "limit": 1 }),
    )
    .await;
    let cursor = page["result"]["structuredContent"]["next_cursor"]
        .as_str()
        .expect("a next page");
    let text = text_of(&page);
    assert!(text.contains(&format!("next_cursor={cursor}")), "{text}");

    // A refusal's guidance is not cut mid-sentence.
    let refused = call(&p, &t, "update_page", json!({ "page_id": "markdown/instances/note/c.md", "content": "no frontmatter at all, but long enough to be a body " })).await;
    let text = text_of(&refused);
    assert!(text.starts_with("Refused:"), "{text}");
    assert!(!text.contains('…'), "no truncation of a refusal: {text}");

    // The token is in structuredContent only; the text announces it.
    let minted = call(
        &p,
        &admin,
        "mint_agent_token",
        json!({ "skill": "note", "target_page_id": "markdown/instances/note/a.md" }),
    )
    .await;
    let token = minted["result"]["structuredContent"]["token"]
        .as_str()
        .expect("token")
        .to_owned();
    let text = text_of(&minted);
    assert!(
        !text.contains(&token),
        "a secret is not repeated into the text: {text}"
    );
    assert!(
        text.contains("token minted") && text.contains("expires"),
        "{text}"
    );
}

// ------------------------------------------------------------------------- cursors ---

fn is_invalid_cursor(v: &Value) -> bool {
    v["result"]["isError"] == json!(true) && first_issue(v)["code"] == "invalid_cursor"
}

/// A cursor is signed: one the server did not issue (made up, or a real one with a character
/// changed) is `invalid_cursor` — on every paged list, in the same typed shape.
#[tokio::test]
async fn cursors_are_signed_and_every_list_refuses_a_bad_one_the_same_way() {
    let p = start().await;
    let admin = p.mint_token(TENANT, Role::Admin);
    for id in ["b", "c"] {
        let c = format!("---\nkind: instance\nskill: note\nid: {id}\n---\n# {id}\n");
        let w = call(
            &p,
            &admin,
            "update_page",
            json!({ "page_id": format!("markdown/instances/note/{id}.md"), "content": c }),
        )
        .await;
        assert_eq!(w["result"]["structuredContent"]["ok"], true, "{w}");
    }
    let page = call(
        &p,
        &admin,
        "list_instances",
        json!({ "skill_id": "note", "limit": 1 }),
    )
    .await;
    let real = page["result"]["structuredContent"]["next_cursor"]
        .as_str()
        .expect("cursor")
        .to_owned();
    let next = call(
        &p,
        &admin,
        "list_instances",
        json!({ "skill_id": "note", "limit": 1, "cursor": real }),
    )
    .await;
    assert_eq!(
        next["result"]["isError"],
        json!(false),
        "the issued cursor works: {next}"
    );

    // Tampered: flip the last character of the issued token.
    let mut tampered = real.clone();
    let last = tampered.pop().unwrap();
    tampered.push(if last == 'A' { 'B' } else { 'A' });
    let r = call(
        &p,
        &admin,
        "list_instances",
        json!({ "skill_id": "note", "limit": 1, "cursor": tampered }),
    )
    .await;
    assert!(is_invalid_cursor(&r), "{r}");

    // Made up: the unsigned base64 of a plausible key (what used to be accepted).
    use base64::Engine as _;
    let forged =
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode("|markdown/instances/note/a.md");
    for (tool, args) in [
        (
            "list_instances",
            json!({ "skill_id": "note", "cursor": forged }),
        ),
        ("list_inbox", json!({ "cursor": forged })),
        (
            "list_events",
            json!({ "label_skill": "note", "cursor": forged }),
        ),
        ("list_drafts", json!({ "cursor": forged })),
        ("list_changesets", json!({ "cursor": "k1.Zm9yZ2Vk" })),
        (
            "list_messages",
            json!({ "chat_group_id": "g", "cursor": forged }),
        ),
    ] {
        let r = call(&p, &admin, tool, args).await;
        assert!(is_invalid_cursor(&r), "{tool}: {r}");
    }
}

// ------------------------------------------------------------------- row ergonomics ---

/// A row's values are returned once: `source` carries them, the schema and the raw row are behind
/// `include_schema`, the source-owned frontmatter fields are named, and `direct_write: false` says
/// what `read_only: true` always meant (rows change through a `write_back` draft).
#[tokio::test]
async fn expanding_a_row_returns_each_value_once() {
    let g = Gw::start().await;
    let slim = g.admin("expand", json!({ "page_id": row_page(2) })).await;
    let bp = &slim["backend_projection"];
    assert!(
        bp.get("columns").is_none() && bp.get("rows").is_none(),
        "{bp}"
    );
    assert_eq!(bp["source"]["sales_doc"], doc(2), "{bp}");
    assert_eq!(bp["direct_write"], false, "{bp}");
    assert_eq!(bp["read_only"], true, "kept for one release: {bp}");
    let ro: Vec<&str> = bp["read_only_fields"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert!(
        ro.contains(&"sales_doc") && ro.contains(&"status"),
        "{ro:?}"
    );
    assert_eq!(bp["writable_via"], "write_back", "{bp}");

    let full = g
        .admin(
            "expand",
            json!({ "page_id": row_page(2), "include_schema": true }),
        )
        .await;
    let bp = &full["backend_projection"];
    assert!(
        bp["columns"].as_array().is_some_and(|c| !c.is_empty()),
        "{bp}"
    );
    assert_eq!(bp["rows"].as_array().map(Vec::len), Some(1), "{bp}");
    assert!(
        slim.to_string().len() < full.to_string().len(),
        "the default answer is the smaller one"
    );
}

/// A body-only write is refused WITH the shape that would have worked (and the `type:` -> `kind:` rename).
#[tokio::test]
async fn a_write_without_frontmatter_is_refused_with_a_minimal_example() {
    let p = start().await;
    let admin = p.mint_token(TENANT, Role::Admin);
    let r = call(
        &p,
        &admin,
        "update_page",
        json!({ "page_id": "markdown/instances/note/z.md", "content": "just a body" }),
    )
    .await;
    let i = first_issue(&r);
    assert_eq!(i["code"], "frontmatter_parse", "{r}");
    let hint = i["suggestion"].as_str().unwrap_or_default();
    assert!(
        hint.contains("kind: instance") && hint.contains("skill: <skill>"),
        "{hint}"
    );
    assert!(hint.contains("`type:`"), "names the retired key: {hint}");
    assert!(
        text_of(&r).contains("kind: instance"),
        "the text carries it too: {}",
        text_of(&r)
    );
}

// ------------------------------------------------------------------- naming and order ---

async fn tools_list(p: &EscurelProcess, token: &str) -> Vec<Value> {
    let v: Value = reqwest::Client::new()
        .post(p.mcp_url())
        .header("authorization", format!("Bearer {token}"))
        .json(&json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    v["result"]["tools"].as_array().cloned().unwrap_or_default()
}

/// `tools/list` is grouped (READ, WRITE, REVIEW, RUNNER, SESSION, ADMIN) and alphabetical inside a
/// group, so an agent scanning ~40 tools finds the one it wants.
#[tokio::test]
async fn tools_list_is_grouped_and_sorted() {
    let p = start().await;
    let admin = p.mint_token(TENANT, Role::Admin);
    let tools = tools_list(&p, &admin).await;
    let order = [
        "[READ]",
        "[WRITE]",
        "[REVIEW]",
        "[RUNNER]",
        "[SESSION]",
        "[ADMIN]",
    ];
    let key = |t: &Value| {
        let d = t["description"].as_str().unwrap();
        (
            order
                .iter()
                .position(|g| d.starts_with(g))
                .expect("every tool has a group tag"),
            t["name"].as_str().unwrap().to_owned(),
        )
    };
    let keys: Vec<_> = tools.iter().map(key).collect();
    let mut sorted = keys.clone();
    sorted.sort();
    assert_eq!(keys, sorted, "grouped, then by name");
}

/// `describe_backend` is `describe_endpoint` (it describes what `register_endpoint` made); the old
/// name still answers for a release. An openapi endpoint gets an answer, not a protocol error.
#[tokio::test]
async fn describe_endpoint_replaces_describe_backend_and_answers_for_openapi() {
    let p = start().await;
    let admin = p.mint_token(TENANT, Role::Admin);
    let names: Vec<String> = tools_list(&p, &admin)
        .await
        .iter()
        .filter_map(|t| t["name"].as_str().map(str::to_owned))
        .collect();
    assert!(names.contains(&"describe_endpoint".to_owned()), "{names:?}");
    assert!(!names.contains(&"describe_backend".to_owned()), "{names:?}");

    let reg = call(
        &p,
        &admin,
        "register_endpoint",
        json!({ "name": "crm", "kind": "openapi", "base_url": "http://127.0.0.1:9/" }),
    )
    .await;
    assert_eq!(reg["result"]["structuredContent"]["ok"], true, "{reg}");
    for tool in ["describe_endpoint", "describe_backend"] {
        let d = call(&p, &admin, tool, json!({ "endpoint": "crm" })).await;
        assert_eq!(d["result"]["isError"], json!(false), "{tool}: {d}");
        assert_eq!(
            d["result"]["structuredContent"]["kind"], "openapi",
            "{tool}: {d}"
        );
    }
}
