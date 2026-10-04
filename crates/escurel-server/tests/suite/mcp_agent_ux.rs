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

// ---- (3)(4) descriptions, groups and annotations ----------------------------------------------

async fn tools(p: &EscurelProcess) -> Vec<Value> {
    let body: Value = reqwest::Client::new()
        .post(p.mcp_url())
        .json(&json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    body["result"]["tools"].as_array().expect("tools").clone()
}

fn tool<'a>(all: &'a [Value], name: &str) -> &'a Value {
    all.iter()
        .find(|t| t["name"] == name)
        .unwrap_or_else(|| panic!("tool {name}"))
}

#[tokio::test]
async fn every_tool_names_its_group_and_carries_the_four_mcp_annotations() {
    let t = Rows::start().await;
    let all = tools(&t.p).await;
    assert!(all.len() > 40, "the whole surface is listed: {}", all.len());
    for tl in &all {
        let name = tl["name"].as_str().unwrap();
        let desc = tl["description"].as_str().unwrap();
        assert!(
            [
                "[READ]",
                "[WRITE]",
                "[REVIEW]",
                "[RUNNER]",
                "[SESSION]",
                "[ADMIN]"
            ]
            .iter()
            .any(|g| desc.starts_with(g)),
            "{name}: a group tag first, so an agent can pick among 80 tools: {desc}"
        );
        let a = &tl["annotations"];
        for hint in [
            "readOnlyHint",
            "destructiveHint",
            "idempotentHint",
            "openWorldHint",
        ] {
            assert!(a[hint].is_boolean(), "{name}: annotations.{hint}: {tl}");
        }
        if a["readOnlyHint"] == true {
            assert_eq!(a["destructiveHint"], false, "{name}: a read cannot destroy");
        }
    }
    // The ones an agent must not get wrong.
    assert_eq!(
        tool(&all, "list_instances")["annotations"]["readOnlyHint"],
        true
    );
    assert_eq!(tool(&all, "search")["annotations"]["readOnlyHint"], true);
    assert_eq!(
        tool(&all, "delete_page")["annotations"]["destructiveHint"],
        true
    );
    assert_eq!(
        tool(&all, "purge_page")["annotations"]["destructiveHint"],
        true
    );
    assert_eq!(
        tool(&all, "capture_event")["annotations"]["idempotentHint"],
        true
    );
    assert_eq!(tool(&all, "expand")["annotations"]["openWorldHint"], true);
}

#[tokio::test]
async fn the_entry_point_descriptions_say_what_the_tool_returns_and_what_to_do_next() {
    let t = Rows::start().await;
    let all = tools(&t.p).await;
    let d = |n: &str| tool(&all, n)["description"].as_str().unwrap().to_owned();
    let skills = d("list_skills");
    for needle in [
        "START HERE",
        "folder",
        "role",
        "fields",
        "backend",
        "actions",
        "autonomy",
        "writable",
    ] {
        assert!(
            skills.contains(needle),
            "list_skills mentions {needle}: {skills}"
        );
    }
    let inst = d("list_instances");
    assert!(
        inst.contains("rows") && inst.contains("filterable") && inst.contains("trust"),
        "{inst}"
    );
    let cap = d("capture_event");
    assert!(
        cap.contains("label_skill=<action.event>") && cap.contains("instance_page_id"),
        "how to START a skill action: {cap}"
    );
    for reviewer in [
        "promote_draft",
        "discard_draft",
        "promote_changeset",
        "discard_changeset",
        "merge_branch",
    ] {
        assert!(
            d(reviewer).contains("human reviewer action"),
            "{reviewer} says an agent must not decide its own work: {}",
            d(reviewer)
        );
    }
    // `resolve` finally has an output contract.
    assert!(tool(&all, "resolve")["outputSchema"].is_object());
}

// ---- (5) capture_event ------------------------------------------------------------------------

#[tokio::test]
async fn an_event_for_a_label_no_skill_answers_to_is_stored_with_a_warning_that_says_so() {
    let t = Rows::start().await;
    // Known skill: no warning.
    let ok = t
        .call(
            "capture_event",
            json!({ "label_skill": "sales-order", "body": "hello", "mime": "text/plain" }),
        )
        .await;
    assert!(
        ok.get("issues").is_none(),
        "a known label carries no warning: {ok}"
    );
    // Unknown label: it used to become a silent dead inbox event.
    let dead = t
        .call(
            "capture_event",
            json!({ "label_skill": "no-such-skill", "body": "hello", "mime": "text/plain" }),
        )
        .await;
    assert!(
        dead["event_id"].is_string(),
        "the event is still stored: {dead}"
    );
    let w = &dead["issues"][0];
    assert_eq!(w["severity"], "warning", "{dead}");
    assert_eq!(w["code"], "unknown_label_skill", "{dead}");
    assert!(
        w["suggestion"].as_str().unwrap().contains("list_skills"),
        "{dead}"
    );
    // The reserved namespace is the runner's and never warned about.
    let sys = t
        .call(
            "capture_event",
            json!({ "label_skill": "escurel:run-control", "body": "{\"action\":\"pause\"}", "mime": "application/json" }),
        )
        .await;
    assert!(sys.get("issues").is_none() || sys["issues"][0]["code"] != "unknown_label_skill");
}

#[tokio::test]
async fn re_capturing_an_event_id_says_it_was_a_replay() {
    let t = Rows::start().await;
    let args = json!({ "label_skill": "sales-order", "event_id": "evt-replay-1", "body": "x", "mime": "text/plain" });
    let first = t.call("capture_event", args.clone()).await;
    assert!(first.get("replayed").is_none(), "{first}");
    let second = t.call("capture_event", args).await;
    assert_eq!(second["event_id"], first["event_id"]);
    assert_eq!(second["replayed"], true, "{second}");
}

// ---- (6) domain errors on read tools ----------------------------------------------------------

#[tokio::test]
async fn query_instance_with_an_unknown_query_is_a_not_found_refusal_not_an_internal_error() {
    let t = Rows::start().await;
    let body = t
        .rpc("query_instance", json!({ "ref": "[[query::nope]]" }))
        .await;
    let issue = refusal_issue(&body);
    assert_eq!(issue["code"], "query_not_found", "{issue}");
    assert!(
        issue["message"].as_str().unwrap().contains("nope"),
        "{issue}"
    );
}

#[tokio::test]
async fn expand_of_a_page_that_is_not_there_says_what_a_page_id_looks_like() {
    let t = Rows::start().await;
    // A bare id is the common mistake: {page: null} with no hint made an agent guess.
    let bare = t.call("expand", json!({ "page_id": "0004500001" })).await;
    assert!(bare["page"].is_null(), "{bare}");
    let hint = bare["hint"].as_str().expect("a hint for a bare id");
    assert!(
        hint.contains("markdown/instances/") && hint.contains("list_instances"),
        "{hint}"
    );
    // A well-formed id that simply does not exist gets the plain explanation.
    let missing = t
        .call(
            "expand",
            json!({ "page_id": "markdown/instances/sales-order/9999999999.md" }),
        )
        .await;
    assert!(missing["page"].is_null(), "{missing}");
    assert!(missing["hint"].is_string(), "{missing}");
    // A page that exists has no hint.
    let found = t
        .call(
            "expand",
            json!({ "page_id": "markdown/instances/sales-order/0004500001.md" }),
        )
        .await;
    assert!(
        found["page"].is_object() && found.get("hint").is_none(),
        "{found}"
    );
}

#[tokio::test]
async fn a_rows_skill_whose_endpoint_is_not_registered_says_to_ask_an_admin() {
    let t = Rows::start().await;
    let skill = "---\nkind: skill\nid: ghost\ndescription: rows over an endpoint nobody registered\n\
backend:\n  kind: openapi\n  endpoint: ghost_api\n  instances: rows\n  key: $.id\n  \
list:\n    path: /things\n    items: $.data\n  read: { path: \"/things/{id}\" }\n---\n# ghost\n";
    let r = t
        .call(
            "update_page",
            json!({ "page_id": "markdown/skills/ghost.md", "content": skill }),
        )
        .await;
    assert_eq!(r["ok"], true, "{r}");
    let body = t
        .rpc("list_instances", json!({ "skill_id": "ghost" }))
        .await;
    let issue = refusal_issue(&body);
    assert_eq!(issue["code"], "endpoint_not_registered", "{issue}");
    let msg = issue["message"].as_str().unwrap();
    assert!(msg.contains("ghost_api"), "{msg}");
    assert!(
        issue["suggestion"]
            .as_str()
            .unwrap()
            .contains("register_endpoint"),
        "{issue}"
    );
}

// ---- (8) create_draft validates before it judges conflicts --------------------------------------

#[tokio::test]
async fn a_draft_with_invalid_content_gets_the_validation_error_not_a_conflict() {
    let t = Rows::start().await;
    let skill = "---\nkind: skill\nid: note\ndescription: a note\nautonomy: review\n---\n# note\n";
    let r = t
        .call(
            "update_page",
            json!({ "page_id": "markdown/skills/note.md", "content": skill }),
        )
        .await;
    assert_eq!(r["ok"], true, "{r}");
    let page = "markdown/instances/note/n1.md";
    let v0 = "---\nkind: instance\nskill: note\nid: n1\n---\n# n1\n";
    let r = t
        .call("update_page", json!({ "page_id": page, "content": v0 }))
        .await;
    assert_eq!(r["ok"], true, "{r}");
    let head = t.call("expand", json!({ "page_id": page })).await;
    let base = head["content_sha256"]
        .as_str()
        .unwrap_or_else(|| panic!("content_sha256: {head}"))
        .to_owned();
    // A first draft is open.
    let first = t
        .call(
            "create_draft",
            json!({ "target_page_id": page, "content": format!("{v0}more\n"), "base_sha256": base }),
        )
        .await;
    assert_eq!(first["ok"], true, "{first}");
    // A second, INVALID one (legacy `type:`) used to be answered with "a draft is already open".
    let legacy = "---\ntype: instance\nskill: note\nid: n1\n---\n# n1\n";
    let second = t
        .rpc(
            "create_draft",
            json!({ "target_page_id": page, "content": legacy, "base_sha256": base }),
        )
        .await;
    let codes: Vec<String> = second["result"]["structuredContent"]["issues"]
        .as_array()
        .unwrap_or_else(|| panic!("issues: {second}"))
        .iter()
        .map(|i| i["code"].as_str().unwrap().to_owned())
        .collect();
    assert!(
        codes.contains(&"frontmatter_type_removed".to_owned()),
        "the content is judged first: {codes:?}"
    );
    assert!(!codes.contains(&"conflict".to_owned()), "{codes:?}");
    // And the invalid attempt must not have disturbed the open draft.
    let open = t.call("list_drafts", json!({})).await;
    assert_eq!(open["drafts"].as_array().unwrap().len(), 1, "{open}");
}

// ---- (9) pagination: one cursor name, and every list can page --------------------------------

#[tokio::test]
async fn event_listings_say_next_cursor_only_and_has_more_says_whether_rows_follow() {
    let t = Rows::start().await;
    for i in 0..3 {
        t.call(
            "capture_event",
            json!({ "label_skill": "sales-order", "body": format!("e{i}"), "mime": "text/plain" }),
        )
        .await;
    }
    for tool in ["list_inbox", "list_events"] {
        let sel = if tool == "list_events" {
            json!({ "label_skill": "sales-order" })
        } else {
            json!({})
        };
        let with = |extra: Value| {
            let mut a = sel.clone();
            for (k, v) in extra.as_object().unwrap() {
                a[k] = v.clone();
            }
            a
        };
        let first = t.call(tool, with(json!({ "limit": 2 }))).await;
        assert_eq!(
            first["events"].as_array().unwrap().len(),
            2,
            "{tool}: {first}"
        );
        assert!(
            first.get("resume_cursor").is_none(),
            "{tool}: resume_cursor is gone: {first}"
        );
        assert_eq!(first["has_more"], true, "{tool}: {first}");
        let c1 = first["next_cursor"]
            .as_str()
            .expect("next_cursor")
            .to_owned();

        let second = t
            .call(tool, with(json!({ "limit": 2, "cursor": c1 })))
            .await;
        assert_eq!(
            second["events"].as_array().unwrap().len(),
            1,
            "{tool}: {second}"
        );
        assert!(
            second.get("has_more").is_none() || second["has_more"] == false,
            "{tool}: {second}"
        );
        // `next_cursor` is where this page ENDED, so a tail polls from it; a client that pages until
        // null makes one extra call, which comes back empty with a null cursor.
        let c2 = second["next_cursor"]
            .as_str()
            .expect("end-of-page cursor")
            .to_owned();
        let tail = t
            .call(tool, with(json!({ "limit": 2, "cursor": c2 })))
            .await;
        assert_eq!(
            tail["events"].as_array().unwrap().len(),
            0,
            "{tool}: {tail}"
        );
        assert!(tail["next_cursor"].is_null(), "{tool}: {tail}");
    }
}

#[tokio::test]
async fn drafts_changesets_and_branches_page_with_limit_and_next_cursor() {
    let t = Rows::start().await;
    let skill = "---\nkind: skill\nid: note\ndescription: a note\n---\n# note\n";
    t.call(
        "update_page",
        json!({ "page_id": "markdown/skills/note.md", "content": skill }),
    )
    .await;
    for i in 0..3 {
        let page = format!("markdown/instances/note/n{i}.md");
        let c = format!("---\nkind: instance\nskill: note\nid: n{i}\n---\n# n{i}\n");
        let d = t
            .call(
                "create_draft",
                json!({ "target_page_id": page, "content": c, "base_sha256": "" }),
            )
            .await;
        assert_eq!(d["ok"], true, "{d}");
    }
    let mut seen = 0;
    let mut cursor: Option<String> = None;
    for _ in 0..5 {
        let page = t
            .call("list_drafts", json!({ "limit": 2, "cursor": cursor }))
            .await;
        seen += page["drafts"].as_array().unwrap().len();
        match page["next_cursor"].as_str() {
            Some(c) => cursor = Some(c.to_owned()),
            None => break,
        }
    }
    assert_eq!(seen, 3, "every draft, across pages");
    for tool in ["list_changesets", "list_branches"] {
        let page = t.call(tool, json!({ "limit": 1 })).await;
        assert!(
            page.get("next_cursor").is_some() || page.is_object(),
            "{tool}: {page}"
        );
    }
}
