//! Stage 3 of the OKF/knowledge program: a `sql_view` skill can declare `instances: rows`, and then
//! EVERY ROW of the source relation is an instance (identity = the key column), listed lazily, read as
//! typed fields, read-only, with an OPTIONAL linked markdown page (the stored overlay) that agents and
//! humans write instead of the row.
//!
//! Real gateway over `POST /mcp`, real DuckDB + `FsStore`, 2,500 generated JSON rows read through the
//! `json_dir` connector (core DuckDB, no extension), no mocks at any boundary.

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
const ROWS: usize = 2_500;

/// The id of generated row `n` (a SAP-style zero-padded document number).
fn doc(n: usize) -> String {
    format!("{:010}", 4_500_000 + n)
}
fn row_page(n: usize) -> String {
    format!("markdown/instances/sales-order/{}.md", doc(n))
}

struct Rows {
    p: EscurelProcess,
    src: PathBuf,
    _dirs: Vec<TempDir>,
}

/// Write `ROWS` rows as five JSON files of an array each (the shape `read_json_auto` infers a typed
/// schema from: VARCHAR key, DOUBLE value, DATE).
fn write_source(dir: &Path, skip: Option<usize>) {
    std::fs::create_dir_all(dir).unwrap();
    for file in 0..5 {
        let mut rows = Vec::new();
        for n in file * 500..(file + 1) * 500 {
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

async fn call_raw(p: &EscurelProcess, name: &str, args: Value) -> Value {
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
            src,
            _dirs: vec![store_dir, db_dir, src_dir],
        }
    }

    async fn call(&self, name: &str, args: Value) -> Value {
        call_raw(&self.p, name, args).await
    }
}

#[tokio::test]
async fn every_row_is_an_instance_listed_lazily_by_keyset_over_thousands_of_rows() {
    let t = Rows::start().await;
    let mut seen: Vec<String> = Vec::new();
    let mut cursor: Option<String> = None;
    let mut pages = 0;
    loop {
        let r = t
            .call(
                "list_instances",
                json!({ "skill": "sales-order", "limit": 500, "cursor": cursor }),
            )
            .await;
        let rows = r["instances"].as_array().expect("instances");
        assert!(
            rows.len() <= 500,
            "a page honours its limit: {}",
            rows.len()
        );
        for i in rows {
            assert_eq!(i["skill"], "sales-order");
            // The row's projected columns ARE the instance's frontmatter, typed.
            assert!(i["frontmatter"]["sales_doc"].is_string(), "{i}");
            assert!(i["frontmatter"]["net_value"].is_number(), "{i}");
            assert!(
                i["frontmatter"].get("internal_note").is_none(),
                "an unprojected column is not exposed: {i}"
            );
            seen.push(i["page_id"].as_str().unwrap().to_owned());
        }
        pages += 1;
        match r["next_cursor"].as_str() {
            Some(c) => cursor = Some(c.to_owned()),
            None => break,
        }
        assert!(pages < 20, "the cursor must terminate");
    }
    assert_eq!(seen.len(), ROWS, "every row, once");
    assert_eq!(pages, 5, "500 per page over 2,500 rows");
    let mut sorted = seen.clone();
    sorted.sort();
    sorted.dedup();
    assert_eq!(sorted, seen, "ascending by key, no duplicates");
    assert_eq!(seen[0], row_page(0));
    assert_eq!(seen[ROWS - 1], row_page(ROWS - 1));
}

#[tokio::test]
async fn a_filterable_column_narrows_the_list_with_a_bound_parameter() {
    let t = Rows::start().await;
    let r = t
        .call(
            "list_instances",
            json!({ "skill": "sales-order", "limit": 10_000,
                    "frontmatter_key": "sold_to", "frontmatter_value": "1000007" }),
        )
        .await;
    let rows = r["instances"].as_array().unwrap();
    // n % 40 == 7 over 2,500 rows.
    assert_eq!(rows.len(), (0..ROWS).filter(|n| n % 40 == 7).count(), "{r}");
    assert!(
        rows.iter()
            .all(|i| i["frontmatter"]["sold_to"] == "1000007")
    );
    // A value that tries to be SQL is just a value that matches nothing.
    let inj = t
        .call(
            "list_instances",
            json!({ "skill": "sales-order",
                    "frontmatter_key": "sold_to", "frontmatter_value": "x' OR '1'='1" }),
        )
        .await;
    assert_eq!(inj["instances"].as_array().unwrap().len(), 0, "{inj}");
}

#[tokio::test]
async fn a_row_expands_as_typed_fields_with_a_read_only_projection() {
    let t = Rows::start().await;
    let r = t.call("expand", json!({ "page_id": row_page(7) })).await;
    assert_eq!(r["page"]["skill"], "sales-order", "{r}");
    assert_eq!(r["page"]["page_kind"], "instance");
    let fm = &r["frontmatter"];
    assert_eq!(fm["sales_doc"], doc(7));
    assert_eq!(fm["net_value"].as_f64(), Some(107.0));
    assert_eq!(
        fm["created"], "2026-09-08",
        "a DATE column stays a date: {fm}"
    );
    let bp = &r["backend_projection"];
    assert_eq!(bp["read_only"], true, "{bp}");
    assert_eq!(bp["instances"], "rows");
    assert!(bp["fetched_at"].is_string(), "freshness is visible: {bp}");
    assert_eq!(
        bp["rows"].as_array().unwrap().len(),
        1,
        "exactly the row: {bp}"
    );
    assert_eq!(bp["rows"][0]["netwr"].as_f64(), Some(107.0));
    assert_eq!(
        bp["linked"]["exists"], false,
        "no linked markdown yet: {bp}"
    );
    // The schema is DISCOVERED from the source, and unprojected columns are visible here (and only
    // here: they are not instance frontmatter).
    let kinds: std::collections::BTreeMap<&str, &str> = bp["columns"]
        .as_array()
        .expect("columns")
        .iter()
        .map(|c| (c["name"].as_str().unwrap(), c["kind"].as_str().unwrap()))
        .collect();
    assert_eq!(kinds["vbeln"], "string");
    assert_eq!(kinds["netwr"], "float");
    assert_eq!(kinds["erdat"], "date");
}

#[tokio::test]
async fn an_unknown_key_is_absent_and_a_wikilink_to_a_row_resolves() {
    let t = Rows::start().await;
    let gone = t
        .call("expand", json!({ "page_id": row_page(ROWS + 5) }))
        .await;
    assert!(gone["page"].is_null(), "{gone}");
    let hit = t
        .call(
            "resolve",
            json!({ "wikilink": format!("[[sales-order::{}]]", doc(3)) }),
        )
        .await;
    assert_eq!(hit["exists"], true, "{hit}");
    assert_eq!(hit["page"]["page_id"], row_page(3));
    let miss = t
        .call("resolve", json!({ "wikilink": "[[sales-order::nope]]" }))
        .await;
    assert_eq!(miss["exists"], false, "{miss}");
}

fn overlay(n: usize, risk: &str, note: &str) -> String {
    format!(
        "---\nkind: instance\nid: {0}\nskill: sales-order\ndelivery_risk: {risk}\n---\n# {0}\n\n{note}\n",
        doc(n)
    )
}

#[tokio::test]
async fn the_linked_markdown_is_created_lazily_by_the_first_write_and_merged_into_the_row() {
    let t = Rows::start().await;
    let page = row_page(11);
    let w = t
        .call(
            "update_page",
            json!({ "page_id": page, "content": overlay(11, "high", "Supplier moved the date twice.") }),
        )
        .await;
    assert_eq!(w["ok"], true, "the first write creates the companion: {w}");

    let r = t.call("expand", json!({ "page_id": page })).await;
    assert!(
        r["body"].as_str().unwrap().contains("moved the date twice"),
        "the notes are the body: {r}"
    );
    // ONE instance for a reader: the row's columns and the notes' own frontmatter, together.
    assert_eq!(r["frontmatter"]["delivery_risk"], "high", "{r}");
    assert_eq!(r["frontmatter"]["sales_doc"], doc(11), "{r}");
    assert_eq!(r["backend_projection"]["linked"]["exists"], true);
    assert!(
        r["content_sha256"].is_string(),
        "the stored companion is CAS-able: {r}"
    );

    // Nothing was materialised per row: the list is still the rows, once each.
    let all = t
        .call(
            "list_instances",
            json!({ "skill": "sales-order", "limit": 10_000 }),
        )
        .await;
    assert_eq!(all["instances"].as_array().unwrap().len(), ROWS, "{all}");
}

#[tokio::test]
async fn a_write_that_touches_a_source_column_is_refused_not_silently_dropped() {
    let t = Rows::start().await;
    let page = row_page(12);
    let bad = format!(
        "---\nkind: instance\nid: {0}\nskill: sales-order\nnet_value: 1.0\n---\n# {0}\n",
        doc(12)
    );
    let r = t
        .call("update_page", json!({ "page_id": page, "content": bad }))
        .await;
    assert_eq!(r["ok"], false, "{r}");
    assert_eq!(r["issues"][0]["code"], "backend_read_only_field", "{r}");
    assert!(
        r["issues"][0]["message"]
            .as_str()
            .unwrap()
            .contains("net_value"),
        "names the field: {r}"
    );
    // The same rule for a held write: an agent's draft is refused when it is made.
    let d = t
        .call(
            "create_draft",
            json!({ "target_page_id": page, "content": bad, "base_sha256": "" }),
        )
        .await;
    assert_eq!(d["ok"], false, "{d}");
    assert_eq!(d["issues"][0]["code"], "backend_read_only_field", "{d}");
    // And a companion may not repoint the backend binding (the old read-out-a-server-table hole).
    let smuggle = format!(
        "---\nkind: instance\nid: {0}\nskill: sales-order\nbackend_ref: {{kind: sql_view, view: external_credentials}}\n---\n# {0}\n",
        doc(12)
    );
    let s = t
        .call(
            "update_page",
            json!({ "page_id": page, "content": smuggle }),
        )
        .await;
    assert_eq!(s["ok"], false, "{s}");
    // Nothing was created by the refusals.
    let r = t.call("expand", json!({ "page_id": page })).await;
    assert_eq!(r["backend_projection"]["linked"]["exists"], false, "{r}");
}

#[tokio::test]
async fn a_draft_and_its_promotion_change_only_the_markdown_side() {
    let t = Rows::start().await;
    let page = row_page(13);
    let before = t.call("expand", json!({ "page_id": page })).await;
    let draft = t
        .call(
            "create_draft",
            json!({ "target_page_id": page, "base_sha256": "",
                    "content": overlay(13, "medium", "Agent analysis: two orders depend on this vendor.") }),
        )
        .await;
    assert_eq!(draft["ok"], true, "{draft}");
    let draft_id = draft["draft"]["draft_id"]
        .as_str()
        .expect("draft id")
        .to_owned();
    let promoted = t
        .call("promote_draft", json!({ "draft_id": draft_id }))
        .await;
    assert_eq!(promoted["ok"], true, "{promoted}");

    let after = t.call("expand", json!({ "page_id": page })).await;
    assert_eq!(after["frontmatter"]["delivery_risk"], "medium", "{after}");
    assert!(
        after["body"]
            .as_str()
            .unwrap()
            .contains("two orders depend")
    );
    // The row itself is exactly as the source has it.
    assert_eq!(
        after["backend_projection"]["rows"], before["backend_projection"]["rows"],
        "the source row never changes"
    );
    assert_eq!(
        after["frontmatter"]["net_value"],
        before["frontmatter"]["net_value"]
    );
}

#[tokio::test]
async fn the_companion_survives_its_row_and_is_flagged_an_orphan() {
    let t = Rows::start().await;
    let page = row_page(21);
    let w = t
        .call(
            "update_page",
            json!({ "page_id": page, "content": overlay(21, "low", "Notes that must not be lost.") }),
        )
        .await;
    assert_eq!(w["ok"], true, "{w}");

    // The row disappears upstream (the extract is regenerated without it).
    write_source(&t.src, Some(21));

    let r = t.call("expand", json!({ "page_id": page })).await;
    assert_eq!(r["page"]["page_id"], page, "the page is kept: {r}");
    assert!(
        r["body"].as_str().unwrap().contains("must not be lost"),
        "{r}"
    );
    assert_eq!(
        r["backend_projection"]["issue"]["code"], "source_missing",
        "flagged, not hidden: {r}"
    );
    // The list is of live rows: the orphan is not one of them.
    let all = t
        .call(
            "list_instances",
            json!({ "skill": "sales-order", "limit": 10_000 }),
        )
        .await;
    let ids: Vec<&str> = all["instances"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["page_id"].as_str().unwrap())
        .collect();
    assert_eq!(ids.len(), ROWS - 1);
    assert!(!ids.contains(&page.as_str()));
}

// ---- row ACL --------------------------------------------------------------------------------

const ACL_SKILL: &str = "sales-order";

fn acl_skill_page(src: &Path) -> String {
    skill_page(src).replace(
        "description: A sales order, one instance per row of the SAP extract.",
        "description: A sales order, one instance per row of the SAP extract.\nvisibility: owner\nowner_field: sold_to",
    )
}

/// Owner-private rows: a caller sees only the rows whose `sold_to` is their own subject. The filter
/// runs AFTER the fetch, so a page can be short (even empty) while `next_cursor` is still set.
#[tokio::test]
async fn row_acl_filters_after_the_fetch_so_pages_are_short_but_nothing_leaks() {
    use escurel_test_support::{FixtureBuilder, Role};
    let src_dir = TempDir::new().unwrap();
    let src = src_dir.path().join("vbak");
    write_source(&src, None);
    let p = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(
            FixtureBuilder::new()
                .tenant("acme")
                .skill(ACL_SKILL, acl_skill_page(&src))
                .done(),
        ),
        ..Default::default()
    })
    .await;
    let post = |token: String, name: &'static str, args: Value| {
        let url = p.mcp_url();
        async move {
            let body: Value = reqwest::Client::new()
                .post(url)
                .header("authorization", format!("Bearer {token}"))
                .json(&json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                               "params": { "name": name, "arguments": args } }))
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
            assert!(body.get("error").is_none(), "{name}: {body}");
            body["result"]["structuredContent"].clone()
        }
    };
    let alice = "1000005"; // n % 40 == 5
    let tok = p.mint_token_with_sub("acme", Role::Agent, alice);

    let mut mine: Vec<String> = Vec::new();
    let mut short_with_more = false;
    let mut cursor: Option<String> = None;
    for _ in 0..40 {
        let r = post(
            tok.clone(),
            "list_instances",
            json!({ "skill": ACL_SKILL, "limit": 100, "cursor": cursor }),
        )
        .await;
        let page = r["instances"].as_array().unwrap();
        for i in page {
            assert_eq!(i["frontmatter"]["sold_to"], alice, "only her rows: {i}");
            mine.push(i["page_id"].as_str().unwrap().to_owned());
        }
        match r["next_cursor"].as_str() {
            Some(c) => {
                short_with_more |= page.len() < 100;
                cursor = Some(c.to_owned());
            }
            None => break,
        }
    }
    let expected = (0..ROWS).filter(|n| n % 40 == 5).count();
    assert_eq!(mine.len(), expected, "all of hers, none of anyone else's");
    assert!(
        short_with_more,
        "pages are short while the cursor is still set (ACL runs after the fetch)"
    );

    // A direct read of someone else's row is absent, exactly like a missing one; her own reads.
    let foreign = post(tok.clone(), "expand", json!({ "page_id": row_page(6) })).await;
    assert!(foreign["page"].is_null(), "{foreign}");
    let own = post(tok.clone(), "expand", json!({ "page_id": row_page(5) })).await;
    assert_eq!(own["page"]["skill"], ACL_SKILL, "{own}");
    let link = post(
        tok,
        "resolve",
        json!({ "wikilink": format!("[[{ACL_SKILL}::{}]]", doc(6)) }),
    )
    .await;
    assert_eq!(link["exists"], false, "no existence oracle: {link}");
}

#[tokio::test]
async fn a_malformed_cursor_gets_an_answer_not_a_dropped_connection() {
    // `aéb` is an even number of BYTES but cuts a multi-byte character: slicing it as hex used to
    // panic inside the handler and the client saw a dead connection instead of an error.
    let t = Rows::start().await;
    let resp = reqwest::Client::new()
        .post(t.p.mcp_url())
        .json(&json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                       "params": { "name": "list_instances",
                                   "arguments": { "skill": "sales-order", "cursor": "aéb" } } }))
        .send()
        .await
        .expect("the gateway must answer, not drop the connection");
    let body: Value = resp.json().await.expect("a JSON-RPC answer");
    // A worded refusal an agent can act on (restart without the cursor), never a dropped connection.
    assert_eq!(
        body["result"]["structuredContent"]["issues"][0]["code"], "invalid_cursor",
        "a bad cursor is a refusal: {body}"
    );
    // The gateway is still healthy afterwards.
    let r = t
        .call(
            "list_instances",
            json!({ "skill": "sales-order", "limit": 2 }),
        )
        .await;
    assert_eq!(r["instances"].as_array().map(Vec::len), Some(2), "{r}");
}

#[tokio::test]
async fn source_rows_say_where_their_values_came_from() {
    // SQL rows are not instructions either: every list item and every projection is marked `source`
    // (the REST/MCP rows are marked `external`), so an agent can tell record data from authored text.
    let t = Rows::start().await;
    let listed = t
        .call(
            "list_instances",
            json!({ "skill": "sales-order", "limit": 3 }),
        )
        .await;
    for i in listed["instances"].as_array().expect("instances") {
        assert_eq!(i["trust"], "source", "a listed row is marked: {i}");
    }
    let page = t.call("expand", json!({ "page_id": row_page(7) })).await;
    assert_eq!(
        page["backend_projection"]["trust"], "source",
        "an expanded row's projection is marked: {page}"
    );
}

/// `search` reaches into rows-backed skills, honestly and bounded: a query matches the KEY and the
/// declared `filterable:` columns (never an undeclared column), a bound parameter does the matching,
/// the page is capped, and the ACL runs after the fetch. Rows were invisible to `search` before.
#[tokio::test]
async fn search_finds_rows_by_key_and_by_a_filterable_column_and_nothing_else() {
    let t = Rows::start().await;

    // By the KEY: one row, a page id the rest of the surface can open.
    let by_key = t
        .call(
            "search",
            json!({ "q": "4501234", "page_kind": "instance", "k": 10 }),
        )
        .await;
    let hits = by_key["hits"].as_array().unwrap();
    assert_eq!(hits.len(), 1, "{by_key}");
    assert_eq!(hits[0]["page_id"], row_page(1234), "{by_key}");
    assert_eq!(hits[0]["skill"], "sales-order", "{by_key}");
    assert_eq!(hits[0]["page_kind"], "instance", "{by_key}");

    // By a FILTERABLE column (`kunnr`, shown as `sold_to`): 62 rows match; the page is capped at `k`.
    let by_col = t
        .call(
            "search",
            json!({ "q": "1000007", "page_kind": "instance", "k": 5 }),
        )
        .await;
    let hits = by_col["hits"].as_array().unwrap();
    assert_eq!(hits.len(), 5, "capped at k: {by_col}");
    for h in hits {
        assert_eq!(h["skill"], "sales-order", "{h}");
        assert_eq!(h["frontmatter_excerpt"]["sold_to"], "1000007", "{h}");
    }

    // A column the skill did NOT declare searchable never matches, and never leaks.
    let hidden = t
        .call(
            "search",
            json!({ "q": "never on the wire", "page_kind": "instance", "k": 10 }),
        )
        .await;
    assert!(
        hidden["hits"]
            .as_array()
            .unwrap()
            .iter()
            .all(|h| h["skill"] != "sales-order"),
        "an undeclared column is not searchable: {hidden}"
    );

    // Restricting to skills, or to another skill, brings no rows.
    for args in [
        json!({ "q": "4501234", "page_kind": "skill", "k": 10 }),
        json!({ "q": "4501234", "skill": "other-skill", "k": 10 }),
    ] {
        let none = t.call("search", args.clone()).await;
        assert!(
            none["hits"]
                .as_array()
                .unwrap()
                .iter()
                .all(|h| h["page_kind"] != "instance" || h["skill"] != "sales-order"),
            "{args}: {none}"
        );
    }
}

/// The same lookup for an owner-private skill: the row ACL runs AFTER the fetch, so a caller finds
/// only their own rows however they phrase the query.
#[tokio::test]
async fn search_over_rows_never_returns_a_row_the_caller_may_not_read() {
    use escurel_test_support::{FixtureBuilder, Role};
    let src_dir = TempDir::new().unwrap();
    let src = src_dir.path().join("vbak");
    write_source(&src, None);
    let p = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(
            FixtureBuilder::new()
                .tenant("acme")
                .skill(ACL_SKILL, acl_skill_page(&src))
                .done(),
        ),
        ..Default::default()
    })
    .await;
    let tok = p.mint_token_with_sub("acme", Role::Agent, "1000005");
    let search = |q: &'static str| {
        let url = p.mcp_url();
        let tok = tok.clone();
        async move {
            let body: Value = reqwest::Client::new()
                .post(url)
                .header("authorization", format!("Bearer {tok}"))
                .json(&json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                    "params": { "name": "search",
                                "arguments": { "q": q, "page_kind": "instance", "k": 50 } } }))
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
            assert!(body.get("error").is_none(), "search: {body}");
            body["result"]["structuredContent"]["hits"].clone()
        }
    };
    // Someone else's customer number finds nothing of hers; hers finds only hers.
    assert!(
        search("1000006").await.as_array().unwrap().is_empty(),
        "no foreign row"
    );
    let mine = search("1000005").await;
    let mine = mine.as_array().unwrap();
    assert!(!mine.is_empty(), "her own rows are found");
    for h in mine {
        assert_eq!(h["frontmatter_excerpt"]["sold_to"], "1000005", "{h}");
    }
}

/// `neighbours` over rows: the notes of a row link to other rows, and a row nobody wrote notes for is
/// still reachable from the pages that link to it (it has no stored page, only a key).
#[tokio::test]
async fn neighbours_follow_links_from_a_rows_notes_and_into_a_row_without_a_stored_page() {
    let t = Rows::start().await;
    let notes = format!(
        "---\nkind: instance\nid: {0}\nskill: sales-order\n---\n# {0}\n\nSee also [[sales-order::{1}]].\n",
        doc(11),
        doc(12)
    );
    let w = t
        .call(
            "update_page",
            json!({ "page_id": row_page(11), "content": notes }),
        )
        .await;
    assert_eq!(w["ok"], true, "{w}");

    // OUT of the notes: the row they point at.
    let out = t
        .call(
            "neighbours",
            json!({ "page_id": row_page(11), "direction": "out" }),
        )
        .await;
    let edges = out["edges"].as_array().unwrap();
    assert!(
        edges
            .iter()
            .any(|e| e["dst_page"] == doc(12) && e["link_skill"] == "sales-order"),
        "the notes link to row 12: {out}"
    );

    // INTO a row that has no stored page: who points at it.
    let into = t
        .call(
            "neighbours",
            json!({ "page_id": row_page(12), "direction": "in" }),
        )
        .await;
    let edges = into["edges"].as_array().unwrap();
    assert!(
        edges.iter().any(|e| e["src_page"] == row_page(11)),
        "row 12 has no stored page but is linked from row 11's notes: {into}"
    );
}

/// An edge to an owner-private row is dropped for a caller who may not read that row, exactly like a
/// direct read: the link must not reveal that a foreign row exists.
#[tokio::test]
async fn neighbours_do_not_reveal_a_link_to_a_row_the_caller_may_not_read() {
    use escurel_test_support::{FixtureBuilder, Role};
    let src_dir = TempDir::new().unwrap();
    let src = src_dir.path().join("vbak");
    write_source(&src, None);
    let note_skill =
        "---\nkind: skill\nid: note\ndescription: A free note.\nautonomy: auto\n---\n# note\n";
    let p = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(
            FixtureBuilder::new()
                .tenant("acme")
                .skill(ACL_SKILL, acl_skill_page(&src))
                .skill("note", note_skill)
                .done(),
        ),
        ..Default::default()
    })
    .await;
    let tok = p.mint_token_with_sub("acme", Role::Agent, "1000005");
    let post = |name: &'static str, args: Value| {
        let url = p.mcp_url();
        let tok = tok.clone();
        async move {
            let body: Value = reqwest::Client::new()
                .post(url)
                .header("authorization", format!("Bearer {tok}"))
                .json(&json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                               "params": { "name": name, "arguments": args } }))
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
            assert!(body.get("error").is_none(), "{name}: {body}");
            body["result"]["structuredContent"].clone()
        }
    };
    let page = "markdown/instances/note/n1.md";
    let content = format!(
        "---\nkind: instance\nid: n1\nskill: note\n---\n# n1\n\nMine [[sales-order::{}]], not mine [[sales-order::{}]].\n",
        doc(5),
        doc(6)
    );
    let w = post(
        "update_page",
        json!({ "page_id": page, "content": content }),
    )
    .await;
    assert_eq!(w["ok"], true, "{w}");
    let out = post("neighbours", json!({ "page_id": page, "direction": "out" })).await;
    let dsts: Vec<&str> = out["edges"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|e| e["dst_page"].as_str())
        .collect();
    assert!(
        dsts.contains(&doc(5).as_str()),
        "her own row is linked: {out}"
    );
    assert!(
        !dsts.contains(&doc(6).as_str()),
        "a foreign row's link is not revealed: {out}"
    );
}
