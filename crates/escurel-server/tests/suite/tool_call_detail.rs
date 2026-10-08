//! What a run's recorded tool calls keep beyond sizes (`ESCUREL_TOOLCALL_DETAIL`): a
//! bounded, redacted summary of the arguments and the result, readable only by someone who may
//! read the run. Real gateway, real DuckDB, raw JSON-RPC; the calls are made by a run-bound
//! machine token.

use escurel_test_support::{
    AuthMode, ConfigOverrides, EscurelProcess, EventAclMode, FixtureBuilder, Opts, Role,
    ToolcallDetailMode,
};
use serde_json::{Value, json};

const TENANT: &str = "carl";
const RUN: &str = "01HRUNTOOLDETAIL0000000000";
const ROOT: &str = "01HROOTTOOLDETAIL000000000";
const PAGE: &str = "markdown/instances/note/n1.md";
const SKILL: &str = "---\nkind: skill\nid: note\ndescription: d.\n---\n# note\n";
const NOTE: &str = "---\nkind: instance\nid: n1\nskill: note\n---\n# n1\n";

async fn start(detail: Option<ToolcallDetailMode>, acl: EventAclMode) -> EscurelProcess {
    EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        config_overrides: ConfigOverrides {
            event_acl: Some(acl),
            toolcall_detail: detail,
            ..Default::default()
        },
        fixtures: Some(
            FixtureBuilder::new()
                .tenant(TENANT)
                .skill("note", SKILL)
                .instance("note", "n1", NOTE)
                .done(),
        ),
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

/// The run exists in the lineage (root + run-started on PAGE) and its machine token.
async fn with_run(p: &EscurelProcess) -> (String, String) {
    let admin = p.mint_token(TENANT, Role::Admin);
    let agent = p.mint_token_for_run(TENANT, Role::Agent, "agent:note", RUN, ROOT);
    call(
        p,
        &admin,
        "capture_event",
        json!({ "event_id": ROOT, "source": "t", "mime": "text/plain", "label_skill": "note",
                "instance_page_id": PAGE, "title": "root", "body": "" }),
    )
    .await;
    call(
        p,
        &admin,
        "capture_event",
        json!({ "event_id": format!("run:{RUN}:started"), "kind": "system", "source": "escurel-runner",
                "mime": "application/json", "label_skill": "escurel:run", "title": "run-started",
                "instance_page_id": PAGE, "body": "{}",
                "provenance": { "runner": { "run_id": RUN, "root_event_id": ROOT, "event_id": ROOT, "harness": "echo" } } }),
    )
    .await;
    (admin, agent)
}

async fn calls(p: &EscurelProcess, token: &str) -> Vec<Value> {
    call(p, token, "get_run_tool_calls", json!({ "run_id": RUN })).await["calls"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

#[tokio::test]
async fn a_call_keeps_what_was_asked_and_what_came_back() {
    let p = start(None, EventAclMode::Off).await;
    let (admin, agent) = with_run(&p).await;
    call(&p, &agent, "expand", json!({ "page_id": PAGE })).await;
    // A refusal keeps its reason.
    let refused = rpc(&p, &agent, "resolve", json!({ "wikilink": 42 })).await;
    assert!(refused.get("error").is_some());

    let rows = calls(&p, &admin).await;
    let expand = rows.iter().find(|c| c["tool"] == "expand").expect("expand");
    let args = expand["args_summary"].as_str().expect("args_summary");
    assert!(args.contains(PAGE), "the page asked for is kept: {args}");
    let result = expand["result_summary"].as_str().expect("result_summary");
    assert!(
        result.contains("note") && result.len() <= 2048 + 64,
        "the answer is summarised, bounded: {result}"
    );
    let resolve = rows
        .iter()
        .find(|c| c["tool"] == "resolve")
        .expect("resolve");
    assert!(
        resolve["result_summary"]
            .as_str()
            .is_some_and(|s| !s.is_empty()),
        "a failure keeps its reason: {resolve}"
    );
}

#[tokio::test]
async fn secrets_never_reach_the_summary() {
    let p = start(None, EventAclMode::Off).await;
    let (admin, agent) = with_run(&p).await;
    call(
        &p,
        &agent,
        "capture_event",
        json!({ "source": "t", "mime": "text/plain", "label_skill": "note",
                "instance_page_id": PAGE,
                "title": "see Bearer PLANTEDBEARER99 and eyJhbGciOiJIUzI1NiJ9.PLANTEDJWTBODY.sig here",
                "body": "",
                "provenance": { "api_token": "PLANTED-API-TOKEN", "secret_ref": "env:PLANTED_SECRET_REF",
                                "nested": { "Authorization": "PLANTED-AUTH", "password": "PLANTED-PW" },
                                "client_secret": "PLANTED-CS", "page": PAGE } }),
    )
    .await;
    let rows = calls(&p, &admin).await;
    let row = rows
        .iter()
        .find(|c| c["tool"] == "capture_event")
        .expect("capture_event");
    let all = row.to_string();
    for planted in [
        "PLANTEDBEARER99",
        "PLANTEDJWTBODY",
        "PLANTED-API-TOKEN",
        "PLANTED_SECRET_REF",
        "PLANTED-AUTH",
        "PLANTED-PW",
        "PLANTED-CS",
    ] {
        assert!(!all.contains(planted), "{planted} leaked: {all}");
    }
    let args = row["args_summary"].as_str().expect("args_summary");
    assert!(args.contains("[redacted]"), "{args}");
    assert!(args.contains(PAGE), "ids stay: {args}");
}

#[tokio::test]
async fn a_summary_is_bounded_and_cut_on_a_character_boundary() {
    let p = start(None, EventAclMode::Off).await;
    let (admin, agent) = with_run(&p).await;
    // 400 multi-byte keys: far more than the cap, and every byte boundary near it is inside a character.
    let mut prov = serde_json::Map::new();
    for i in 0..400 {
        prov.insert(format!("é{i}ü"), json!("ß".repeat(40)));
    }
    call(
        &p,
        &agent,
        "capture_event",
        json!({ "source": "t", "mime": "text/plain", "label_skill": "note",
                "instance_page_id": PAGE, "title": "big", "body": "x".repeat(50_000),
                "provenance": prov }),
    )
    .await;
    let rows = calls(&p, &admin).await;
    let row = rows.iter().find(|c| c["tool"] == "capture_event").unwrap();
    let args = row["args_summary"].as_str().expect("args_summary");
    assert!(args.len() <= 2048 + 32, "capped: {} bytes", args.len());
    assert!(
        args.ends_with("[truncated]"),
        "marked: …{}",
        &args[args.len() - 20..]
    );
    // The 50 kB body is a size, not content.
    assert!(!args.contains("xxxxxxxxxx"), "{args}");
}

#[tokio::test]
async fn off_records_sizes_only() {
    let p = start(Some(ToolcallDetailMode::Off), EventAclMode::Off).await;
    let (admin, agent) = with_run(&p).await;
    call(&p, &agent, "expand", json!({ "page_id": PAGE })).await;
    let rows = calls(&p, &admin).await;
    let expand = rows.iter().find(|c| c["tool"] == "expand").expect("expand");
    assert!(expand["request_bytes"].as_u64().unwrap() > 0, "{expand}");
    assert!(
        expand["args_summary"].is_null() && expand["result_summary"].is_null(),
        "off keeps no detail: {expand}"
    );
}

#[tokio::test]
async fn only_someone_who_may_read_the_run_reads_its_summaries() {
    let p = start(None, EventAclMode::Enforce).await;
    let admin = p.mint_token(TENANT, Role::Admin);
    let agent = p.mint_token_for_run(TENANT, Role::Agent, "agent:note", RUN, ROOT);
    let bob = p.mint_token_with_sub(TENANT, Role::Agent, "bob");
    // An UNASSIGNED run-started (no target page) is readable by admin only.
    call(
        &p,
        &admin,
        "capture_event",
        json!({ "event_id": format!("run:{RUN}:started"), "kind": "system", "source": "escurel-runner",
                "mime": "application/json", "label_skill": "escurel:run", "title": "run-started",
                "body": "{}",
                "provenance": { "runner": { "run_id": RUN, "root_event_id": ROOT, "event_id": ROOT } } }),
    )
    .await;
    call(&p, &agent, "expand", json!({ "page_id": PAGE })).await;

    let theirs = call(&p, &bob, "get_run_tool_calls", json!({ "run_id": RUN })).await;
    assert_eq!(theirs["calls"], json!([]), "{theirs}");
    assert!(!theirs.to_string().contains(PAGE), "{theirs}");
    let mine = calls(&p, &admin).await;
    assert!(
        mine.iter()
            .any(|c| c["args_summary"].as_str().is_some_and(|s| s.contains(PAGE))),
        "an admin sees it: {mine:?}"
    );
}
