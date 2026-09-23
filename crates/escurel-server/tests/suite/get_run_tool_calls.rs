//! `get_run_tool_calls` and the per-run tool-call summary on `list_lineage`
//! (knowledge-workbench backend P3-2). Real gateway, real DuckDB, raw
//! JSON-RPC; the rows come from P3-1's hook on real run-bound calls.

use escurel_test_support::{AuthMode, EscurelProcess, FixtureBuilder, Opts, Role};
use serde_json::{Value, json};

const TENANT: &str = "carl";
const RUN: &str = "01HRUNTOOLCALLS00000000000";
const ROOT: &str = "01HROOTTOOLCALLS0000000000";
const PAGE: &str = "markdown/instances/note/n1.md";
const SKILL: &str = "---\ntype: skill\nid: note\ndescription: d.\n---\n# note\n";
const NOTE: &str = "---\ntype: instance\nid: n1\nskill: note\n---\n# n1\n";

async fn start() -> EscurelProcess {
    EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(
            FixtureBuilder::new()
                .tenant(TENANT)
                .skill("note", SKILL)
                .instance("note", "n1", NOTE)
                .done(),
        ),
        ..Default::default()
    })
    .await
}

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

async fn call(p: &EscurelProcess, token: &str, name: &str, args: Value) -> Value {
    let body = rpc(p, token, name, args).await;
    assert!(body.get("error").is_none(), "{name}: {body}");
    body["result"]["structuredContent"].clone()
}

#[tokio::test]
async fn a_runs_tool_calls_are_readable_per_run_and_summarised_on_its_lineage_node() {
    let p = start().await;
    let admin = p.mint_token(TENANT, Role::Admin);
    let agent = p.mint_token_for_run(TENANT, Role::Agent, "agent:note", RUN, ROOT);

    // The run exists in the lineage: a root event and its run-started.
    let r = call(
        &p,
        &admin,
        "capture_event",
        json!({ "event_id": ROOT, "source": "t", "mime": "text/plain", "label_skill": "note",
                "instance_page_id": PAGE, "title": "root", "body": "" }),
    )
    .await;
    assert_eq!(r["event_id"], ROOT);
    call(
        &p,
        &admin,
        "capture_event",
        json!({ "event_id": format!("run:{RUN}:started"), "kind": "system", "source": "escurel-runner",
                "mime": "application/json", "label_skill": "escurel:run", "title": "run-started",
                "instance_page_id": PAGE, "body": "{}",
                "provenance": { "runner": { "run_id": RUN, "root_event_id": ROOT, "event_id": ROOT, "harness": "echo" } } }),
    )
    .await;

    // Three calls by the run: two fine, one refused.
    call(&p, &agent, "list_skills", json!({})).await;
    call(&p, &agent, "expand", json!({ "page_id": PAGE })).await;
    let refused = rpc(&p, &agent, "resolve", json!({ "wikilink": 42 })).await;
    assert!(refused.get("error").is_some());

    // Per run, oldest first, paged by `after`.
    let page = call(&p, &admin, "get_run_tool_calls", json!({ "run_id": RUN })).await;
    let tools: Vec<(String, String)> = page["calls"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| {
            (
                c["tool"].as_str().unwrap().to_owned(),
                c["status"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    assert_eq!(
        tools,
        [
            ("list_skills".to_owned(), "ok".to_owned()),
            ("expand".to_owned(), "ok".to_owned()),
            ("resolve".to_owned(), "error".to_owned())
        ],
        "{page}"
    );
    assert_eq!(page["run_id"], RUN);
    assert!(page["next_after"].is_null(), "{page}");
    let c = &page["calls"][0];
    assert!(
        c["seq"].is_number()
            && c["duration_ms"].is_number()
            && c["response_bytes"].as_u64().unwrap() > 0,
        "{c}"
    );
    assert_eq!(c["subject"], "agent:note");
    let first = call(
        &p,
        &admin,
        "get_run_tool_calls",
        json!({ "run_id": RUN, "limit": 2 }),
    )
    .await;
    assert_eq!(first["calls"].as_array().unwrap().len(), 2);
    let after = first["next_after"].as_i64().expect("more");
    let rest = call(
        &p,
        &admin,
        "get_run_tool_calls",
        json!({ "run_id": RUN, "after": after }),
    )
    .await;
    assert_eq!(rest["calls"][0]["tool"], "resolve");
    // The agent may read its own run's calls too.
    let own = call(&p, &agent, "get_run_tool_calls", json!({ "run_id": RUN })).await;
    assert_eq!(own["calls"].as_array().unwrap().len(), 3);
    // An unknown run is empty, not an error.
    let none = call(
        &p,
        &admin,
        "get_run_tool_calls",
        json!({ "run_id": "01HNOSUCHRUN00000000000000" }),
    )
    .await;
    assert_eq!(none["calls"], json!([]), "{none}");

    // The lineage's run node carries a summary when asked for.
    let tree = call(
        &p,
        &admin,
        "list_lineage",
        json!({ "root_event_id": ROOT, "include": ["runs", "tool_calls"] }),
    )
    .await;
    let run = tree["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|n| n["id"] == RUN)
        .cloned()
        .unwrap_or_else(|| panic!("{tree}"));
    // Four: the three above plus the agent's own `get_run_tool_calls` read,
    // which is a run-bound call like any other.
    assert_eq!(run["tool_call_summary"]["count"], 4, "{run}");
    assert_eq!(run["tool_call_summary"]["failed"], 1, "{run}");
    assert!(run["tool_call_summary"]["duration_ms"].is_number(), "{run}");
    let plain = call(&p, &admin, "list_lineage", json!({ "root_event_id": ROOT })).await;
    let run = plain["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|n| n["id"] == RUN)
        .cloned()
        .unwrap();
    assert!(
        run.get("tool_call_summary").is_none(),
        "only when asked: {run}"
    );
}
