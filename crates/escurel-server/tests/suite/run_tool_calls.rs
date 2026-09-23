//! `run_tool_calls` (knowledge-workbench backend P3-1): every `/mcp` call
//! made with a run-bound bearer leaves one row — the tool, how it went, how
//! long, how big (bytes only) — and an ordinary call leaves none. Read here
//! straight off the indexer the gateway was handed (the read tool is P3-2).
//! Real gateway, real DuckDB.

use std::sync::Arc;

use duckdb::Connection;
use escurel_embed::{Embedder, ZeroEmbedder};
use escurel_index::{Indexer, Migrator};
use escurel_storage::{FsStore, LaneStore};
use escurel_test_support::{AuthMode, ConfigOverrides, EscurelProcess, Opts, Role};
use serde_json::{Value, json};
use tempfile::TempDir;

const TENANT: &str = "carl";
const RUN: &str = "01HRUNTOOLCALLS00000000000";
const ROOT: &str = "01HROOTTOOLCALLS0000000000";
const SKILL: &str = "---\ntype: skill\nid: note\ndescription: d.\n---\n# note\n";

async fn rpc(p: &EscurelProcess, token: &str, name: &str, args: Value) -> Value {
    reqwest::Client::new()
        .post(p.mcp_url())
        .header("authorization", format!("Bearer {token}"))
        .json(&json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                       "params": { "name": name, "arguments": args } }))
        .send()
        .await
        .expect("post")
        .json()
        .await
        .expect("json")
}

#[tokio::test]
async fn a_run_bound_call_leaves_a_row_and_an_ordinary_call_does_not() {
    let store_dir = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    let store: Arc<dyn LaneStore> = Arc::new(FsStore::new(store_dir.path().to_path_buf()));
    let embedder: Arc<dyn Embedder> = Arc::new(ZeroEmbedder::default());
    let conn = Connection::open(db_dir.path().join("escurel.duckdb")).unwrap();
    Migrator::up(&conn).unwrap();
    let indexer = Arc::new(Indexer::new(Arc::clone(&store), embedder, conn, TENANT).unwrap());
    indexer
        .update_page("markdown/skills/note.md", SKILL)
        .await
        .unwrap();
    let p = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        config_overrides: ConfigOverrides {
            indexer: Some(Arc::clone(&indexer)),
            ..Default::default()
        },
        ..Default::default()
    })
    .await;
    let agent = p.mint_token_for_run(TENANT, Role::Agent, "agent:note", RUN, ROOT);
    let plain = p.mint_token(TENANT, Role::Agent);

    // Two run-bound calls: one succeeds, one is refused (a bad argument).
    let ok = rpc(&p, &agent, "list_skills", json!({})).await;
    assert!(ok.get("error").is_none(), "{ok}");
    let err = rpc(&p, &agent, "resolve", json!({ "wikilink": 42 })).await;
    assert!(err.get("error").is_some(), "{err}");
    // A write the gateway REJECTED (`ok: false` — here a page with no
    // frontmatter at all) is neither `ok` nor a JSON-RPC error: it is
    // recorded `rejected`, and counts as failed (live smoke of P3).
    let rej = rpc(
        &p,
        &agent,
        "update_page",
        json!({ "page_id": "markdown/instances/note/n9.md", "content": "no frontmatter\n" }),
    )
    .await;
    assert!(rej.get("error").is_none(), "{rej}");
    // An ordinary call leaves nothing.
    let _ = rpc(&p, &plain, "list_skills", json!({})).await;

    let page = indexer.list_run_tool_calls(RUN, 100, None).await.unwrap();
    let tools: Vec<(&str, &str)> = page
        .calls
        .iter()
        .map(|c| (c.tool.as_str(), c.status.as_str()))
        .collect();
    assert_eq!(
        tools,
        [
            ("list_skills", "ok"),
            ("resolve", "error"),
            ("update_page", "rejected")
        ],
        "{page:?}"
    );
    let summary = indexer
        .run_tool_call_summaries(&[RUN.to_owned()])
        .await
        .unwrap();
    assert_eq!(summary[RUN].count, 3, "{summary:?}");
    assert_eq!(
        summary[RUN].failed, 2,
        "an error and a rejection both failed: {summary:?}"
    );
    let first = &page.calls[0];
    assert_eq!(first.run_id, RUN);
    assert_eq!(first.root_event_id.as_deref(), Some(ROOT));
    assert_eq!(first.subject, "agent:note");
    assert!(
        first.response_bytes > 0 && first.duration_ms >= 0.0,
        "{first:?}"
    );
    assert!(first.at.starts_with("20"), "{first:?}");
    let refused = &page.calls[1];
    assert!(refused.request_bytes > 0, "{refused:?}");
    assert!(page.next_after.is_none());
    // The cursor walks.
    let one = indexer.list_run_tool_calls(RUN, 1, None).await.unwrap();
    assert_eq!(one.calls.len(), 1);
    let after = one.next_after.expect("more");
    let rest = indexer
        .list_run_tool_calls(RUN, 1, Some(after))
        .await
        .unwrap();
    assert_eq!(rest.calls[0].tool, "resolve");
    let last = indexer
        .list_run_tool_calls(RUN, 1, rest.next_after)
        .await
        .unwrap();
    assert_eq!(last.calls[0].tool, "update_page");
    assert!(last.next_after.is_none());
}
