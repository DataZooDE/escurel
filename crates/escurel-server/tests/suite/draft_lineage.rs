//! A draft records the RUN that proposed it (knowledge-workbench backend,
//! P1 PR5 — BRD FR-L-3). Real gateway, real DuckDB, raw JSON-RPC.
//!
//! `run_id` / `root_event_id` come from the caller's token — the per-run
//! agent bearer the runner minted (PR4) — never from an argument: the
//! lineage a draft claims is the lineage the runner signed for it. An
//! ordinary bearer proposes an unattributed draft, exactly as before.

use escurel_test_support::{AuthMode, ConfigOverrides, EscurelProcess, FixtureBuilder, Opts, Role};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

const TENANT: &str = "stuttgart-ai";
const NOTE_SKILL: &str = "---\ntype: skill\nid: note\ndescription: A note.\n\
    visibility: public\n---\n# note\n";
const BASE: &str = "---\ntype: instance\nskill: note\nid: plan\n---\n# Plan\nv1 body.\n";
const PAGE: &str = "markdown/instances/note/plan.md";
const RUN: &str = "01HRUNXXXXXXXXXXXXXXXXXXXX";
const ROOT: &str = "01HROOTXXXXXXXXXXXXXXXXXXX";

fn sha(s: &str) -> String {
    format!("{:x}", Sha256::digest(s.as_bytes()))
}

async fn start() -> EscurelProcess {
    EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        config_overrides: ConfigOverrides::default(),
        fixtures: Some(
            FixtureBuilder::new()
                .tenant(TENANT)
                .skill("note", NOTE_SKILL)
                .instance("note", "plan", BASE)
                .done(),
        ),
    })
    .await
}

async fn call(p: &EscurelProcess, token: &str, tool: &str, args: Value) -> Value {
    let body: Value = reqwest::Client::new()
        .post(p.mcp_url())
        .header("authorization", format!("Bearer {token}"))
        .json(&json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": { "name": tool, "arguments": args },
        }))
        .send()
        .await
        .expect("post")
        .json()
        .await
        .expect("json");
    assert!(body.get("error").is_none(), "{tool} error: {body}");
    body["result"]["structuredContent"].clone()
}

fn draft_args(text: &str) -> Value {
    json!({
        "target_page_id": PAGE,
        "content": format!("---\ntype: instance\nskill: note\nid: plan\n---\n# Plan\n{text}\n"),
        "base_sha256": sha(BASE),
    })
}

#[tokio::test]
async fn a_draft_made_with_a_run_bound_token_records_the_run_and_root_event() {
    let p = start().await;
    let agent = p.mint_token_for_run(TENANT, Role::Agent, "agent:note", RUN, ROOT);
    let r = call(&p, &agent, "create_draft", draft_args("v2 from a run.")).await;
    assert_eq!(r["ok"], true, "{r}");
    assert_eq!(r["draft"]["run_id"], RUN, "{r}");
    assert_eq!(r["draft"]["root_event_id"], ROOT, "{r}");
    assert_eq!(r["draft"]["author"], "agent:note");

    let listed = call(&p, &agent, "list_drafts", json!({})).await;
    let d = &listed["drafts"][0];
    assert_eq!(d["run_id"], RUN, "{listed}");
    assert_eq!(d["root_event_id"], ROOT, "{listed}");
}

#[tokio::test]
async fn a_draft_made_with_an_ordinary_token_has_no_run_lineage() {
    let p = start().await;
    let human = p.mint_token(TENANT, Role::Agent);
    let r = call(&p, &human, "create_draft", draft_args("v2 by hand.")).await;
    assert_eq!(r["ok"], true, "{r}");
    assert!(r["draft"]["run_id"].is_null(), "{r}");
    assert!(r["draft"]["root_event_id"].is_null(), "{r}");
}

#[tokio::test]
async fn a_caller_supplied_run_id_argument_is_ignored() {
    // Lineage is what the runner signed, never what the caller typed: a
    // forged `run_id` would file a human's draft under an agent's run.
    let p = start().await;
    let human = p.mint_token(TENANT, Role::Agent);
    let mut args = draft_args("v2 forged.");
    args["run_id"] = json!("forged-run");
    args["root_event_id"] = json!("forged-root");
    let r = call(&p, &human, "create_draft", args).await;
    assert_eq!(r["ok"], true, "{r}");
    assert!(r["draft"]["run_id"].is_null(), "{r}");
    assert!(r["draft"]["root_event_id"].is_null(), "{r}");
}

#[tokio::test]
async fn list_changesets_and_diff_draft_expose_run_lineage() {
    let p = start().await;
    let agent = p.mint_token_for_run(TENANT, Role::Agent, "agent:note", RUN, ROOT);
    let mut args = draft_args("v2 grouped.");
    args["new_changeset"] = json!(true);
    let r = call(&p, &agent, "create_draft", args).await;
    assert_eq!(r["ok"], true, "{r}");
    let draft_id = r["draft"]["draft_id"].as_str().unwrap().to_owned();
    let changeset_id = r["draft"]["changeset_id"].as_str().unwrap().to_owned();

    let sets = call(&p, &agent, "list_changesets", json!({})).await;
    let cs = sets["changesets"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["changeset_id"] == json!(changeset_id))
        .cloned()
        .unwrap_or_default();
    assert_eq!(cs["run_id"], RUN, "{sets}");
    assert_eq!(cs["root_event_id"], ROOT, "{sets}");

    let diff = call(&p, &agent, "diff_draft", json!({ "draft_id": draft_id })).await;
    assert_eq!(diff["ok"], true, "{diff}");
    assert_eq!(diff["run_id"], RUN, "{diff}");
    assert_eq!(diff["root_event_id"], ROOT, "{diff}");
}
