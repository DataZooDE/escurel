//! `list_lineage` — one tenant-scoped read for a thread (knowledge-workbench
//! backend, P1 PR9 — BRD FR-L-1). Real gateway, real DuckDB, raw JSON-RPC.
//!
//! The tree strictly alternates event → run → {changeset → draft | draft |
//! event}: an event's parent is the run that emitted it (null for the
//! root), a run's parent is the event that triggered it, a changeset's the
//! run that proposed it, a draft's its changeset (or its run). Tree folding
//! is the client's job; the tool guarantees ids and parents, prunes a
//! subtree the caller may not read (denial is absence), and pages over the
//! lineage's events with the usual cursor.

use escurel_test_support::{
    AuthMode, ConfigOverrides, EscurelProcess, EventAclMode, FixtureBuilder, Opts, Role,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

const TENANT: &str = "carl";
const NOTE_SKILL: &str = "---\ntype: skill\nid: note\ndescription: A note.\n\
    visibility: public\n---\n# note\n";
const BASE: &str = "---\ntype: instance\nskill: note\nid: plan\n---\n# Plan\nv1 body.\n";
const PAGE: &str = "markdown/instances/note/plan.md";
const ROOT: &str = "01HROOT";
const RUN1: &str = "01HRUN1";
const RUN2: &str = "01HRUN2";
const HOP: &str = "01HHOP";

async fn start_with(mode: EventAclMode) -> EscurelProcess {
    EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        config_overrides: ConfigOverrides {
            event_acl: Some(mode),
            ..Default::default()
        },
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
    let resp: Value = reqwest::Client::new()
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
    resp
}

fn result(resp: &Value) -> &Value {
    assert!(resp.get("error").is_none(), "unexpected error: {resp}");
    &resp["result"]["structuredContent"]
}

fn run_event(run: &str, root: &str, trigger: &str, title: &str, at: &str, body: Value) -> Value {
    json!({
        "kind": "system", "label_skill": "escurel:run", "title": title, "at": at,
        "source": "escurel-runner", "instance_page_id": PAGE, "body": body.to_string(),
        "provenance": { "runner": {
            "run_id": run, "root_event_id": root, "event_id": trigger,
            "harness": "echo", "attempt": 1, "max_attempts": 3, "target_page_id": PAGE,
        } },
    })
}

/// Root R → run 1 (processed) → {changeset → draft, cascade hop H} → run 2
/// (started only). Seeded the way the runner writes it, without a runner.
async fn seed_thread(p: &EscurelProcess) -> (String, String) {
    let admin = p.mint_token(TENANT, Role::Admin);
    let agent = p.mint_token_for_run(TENANT, Role::Agent, "agent:note", RUN1, ROOT);
    let r = call(
        p,
        &admin,
        "capture_event",
        json!({ "event_id": ROOT, "label_skill": "meeting", "title": "kickoff", "at": "2026-09-22T09:00:00Z" }),
    )
    .await;
    result(&r);
    for (title, at, body) in [
        ("run-started", "2026-09-22T09:01:00Z", json!({})),
        (
            "run-attempt",
            "2026-09-22T09:02:00Z",
            json!({ "attempt": 1, "outcome": "ok" }),
        ),
        (
            "run-progress",
            "2026-09-22T09:02:30Z",
            json!({ "plan": [{ "step": "a", "status": "completed" }] }),
        ),
        (
            "run-finished",
            "2026-09-22T09:03:00Z",
            json!({ "status": "processed", "attempts": 1, "summary": "folded", "produced_instance": PAGE }),
        ),
    ] {
        result(
            &call(
                p,
                &admin,
                "capture_event",
                run_event(RUN1, ROOT, ROOT, title, at, body),
            )
            .await,
        );
    }
    let mut args = json!({
        "target_page_id": PAGE,
        "content": "---\ntype: instance\nskill: note\nid: plan\n---\n# Plan\nv2 from run 1.\n",
        "base_sha256": format!("{:x}", Sha256::digest(BASE.as_bytes())),
        "new_changeset": true,
    });
    let r = call(p, &agent, "create_draft", args.take()).await;
    let draft = result(&r)["draft"].clone();
    let draft_id = draft["draft_id"].as_str().unwrap().to_owned();
    let changeset_id = draft["changeset_id"].as_str().unwrap().to_owned();
    // The cascade hop run 1 emitted, and run 2 it triggered (still running).
    result(
        &call(
            p,
            &admin,
            "capture_event",
            json!({
                "event_id": HOP, "label_skill": "decision-record", "title": "hop", "at": "2026-09-22T09:04:00Z",
                "provenance": { "runner": { "root_event_id": ROOT, "parent_event_id": ROOT,
                                            "parent_run_id": RUN1, "depth": 1, "lineage_path": [ROOT] } },
            }),
        )
        .await,
    );
    result(
        &call(
            p,
            &admin,
            "capture_event",
            run_event(
                RUN2,
                ROOT,
                HOP,
                "run-started",
                "2026-09-22T09:05:00Z",
                json!({}),
            ),
        )
        .await,
    );
    (draft_id, changeset_id)
}

fn node<'a>(nodes: &'a [Value], id: &str) -> &'a Value {
    nodes
        .iter()
        .find(|n| n["id"] == id)
        .unwrap_or_else(|| panic!("no node {id} in {nodes:?}"))
}

#[tokio::test]
async fn a_lineage_is_a_tree_of_event_run_changeset_and_draft_nodes() {
    let p = start_with(EventAclMode::Off).await;
    let (draft_id, changeset_id) = seed_thread(&p).await;
    let agent = p.mint_token(TENANT, Role::Agent);
    let r = call(&p, &agent, "list_lineage", json!({ "root_event_id": ROOT })).await;
    let out = result(&r);
    assert_eq!(out["root_event_id"], ROOT);
    let nodes = out["nodes"].as_array().unwrap();
    assert_eq!(nodes.len(), 6, "{nodes:?}");

    let root = node(nodes, ROOT);
    assert_eq!(root["type"], "event");
    assert!(root["parent"].is_null(), "{root}");
    assert_eq!(root["label_skill"], "meeting");
    assert_eq!(root["state"], "inbox");

    let run1 = node(nodes, RUN1);
    assert_eq!(run1["type"], "run");
    assert_eq!(
        run1["parent"], ROOT,
        "a run hangs off the event that triggered it"
    );
    assert_eq!(run1["state"], "processed");
    assert_eq!(run1["harness"], "echo");
    assert_eq!(run1["summary"], "folded");
    assert_eq!(run1["produced_instance"], PAGE);
    assert_eq!(
        run1["plan"][0]["step"], "a",
        "the newest run-progress: {run1}"
    );
    assert_eq!(run1["started_at"], "2026-09-22T09:01:00Z");
    assert_eq!(run1["finished_at"], "2026-09-22T09:03:00Z");

    let cs = node(nodes, &changeset_id);
    assert_eq!(cs["type"], "changeset");
    assert_eq!(cs["parent"], RUN1);
    assert_eq!(cs["state"], "open");
    let d = node(nodes, &draft_id);
    assert_eq!(d["type"], "draft");
    assert_eq!(d["parent"], changeset_id);
    assert_eq!(d["state"], "open");
    assert_eq!(d["target_page_id"], PAGE);

    let hop = node(nodes, HOP);
    assert_eq!(hop["type"], "event");
    assert_eq!(
        hop["parent"], RUN1,
        "a cascade hop hangs off the run that emitted it"
    );
    assert_eq!(hop["parent_event_id"], ROOT);
    assert_eq!(hop["depth"], 1);
    let run2 = node(nodes, RUN2);
    assert_eq!(run2["parent"], HOP);
    assert_eq!(run2["state"], "running", "no run-finished yet: {run2}");
    assert!(out.get("next_cursor").is_none(), "{out}");
}

#[tokio::test]
async fn a_denied_event_prunes_its_subtree() {
    let p = start_with(EventAclMode::Enforce).await;
    let alice = p.mint_token_with_sub(TENANT, Role::Agent, "alice");
    let bob = p.mint_token_with_sub(TENANT, Role::Agent, "bob");
    let admin = p.mint_token(TENANT, Role::Admin);
    // Alice's un-triaged capture is hers alone; the run under it is the
    // runner's (admin) and, unassigned, visible only to admin.
    result(
        &call(
            &p,
            &alice,
            "capture_event",
            json!({ "event_id": ROOT, "label_skill": "meeting", "title": "private" }),
        )
        .await,
    );
    let mut started = run_event(
        RUN1,
        ROOT,
        ROOT,
        "run-started",
        "2026-09-22T09:01:00Z",
        json!({}),
    );
    started.as_object_mut().unwrap().remove("instance_page_id");
    result(&call(&p, &admin, "capture_event", started).await);

    let ids = |out: &Value| -> Vec<String> {
        out["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|n| n["id"].as_str().unwrap().to_owned())
            .collect()
    };
    let r = call(&p, &bob, "list_lineage", json!({ "root_event_id": ROOT })).await;
    assert!(
        ids(result(&r)).is_empty(),
        "an unreadable root prunes everything: {r}"
    );
    let r = call(&p, &alice, "list_lineage", json!({ "root_event_id": ROOT })).await;
    assert_eq!(
        ids(result(&r)),
        vec![ROOT],
        "the run's events are not hers to see: {r}"
    );
    let r = call(&p, &admin, "list_lineage", json!({ "root_event_id": ROOT })).await;
    assert_eq!(ids(result(&r)).len(), 2, "{r}");
}

#[tokio::test]
async fn include_filters_node_types_and_pagination_carries_the_cursor() {
    let p = start_with(EventAclMode::Off).await;
    seed_thread(&p).await;
    let agent = p.mint_token(TENANT, Role::Agent);
    let r = call(
        &p,
        &agent,
        "list_lineage",
        json!({ "root_event_id": ROOT, "include": ["events"] }),
    )
    .await;
    let types: Vec<&str> = result(&r)["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n["type"].as_str().unwrap())
        .collect();
    assert_eq!(types, ["event", "event"], "{r}");
    let r = call(
        &p,
        &agent,
        "list_lineage",
        json!({ "root_event_id": ROOT, "include": ["drafts"] }),
    )
    .await;
    let types: Vec<&str> = result(&r)["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n["type"].as_str().unwrap())
        .collect();
    assert!(
        types.iter().all(|t| *t == "draft" || *t == "changeset"),
        "{r}"
    );

    // Two events per page: the lineage has seven rows (root, five run rows,
    // the hop), so the cursor must come back, and draining collects them all.
    let mut cursor: Option<String> = None;
    let mut seen = Vec::new();
    let mut pages = 0;
    loop {
        let mut args = json!({ "root_event_id": ROOT, "include": ["events", "runs"], "limit": 2 });
        if let Some(c) = &cursor {
            args["cursor"] = json!(c);
        }
        let r = call(&p, &agent, "list_lineage", args).await;
        let out = result(&r).clone();
        for n in out["nodes"].as_array().unwrap() {
            seen.push(n["id"].as_str().unwrap().to_owned());
        }
        pages += 1;
        match out.get("next_cursor").and_then(Value::as_str) {
            Some(c) => cursor = Some(c.to_owned()),
            None => break,
        }
        assert!(pages < 10, "never terminated");
    }
    assert!(pages >= 3, "paged: {pages}");
    for id in [ROOT, RUN1, HOP, RUN2] {
        assert!(seen.contains(&id.to_owned()), "{id} missing from {seen:?}");
    }
}

#[tokio::test]
async fn an_unknown_root_returns_an_empty_tree_not_an_error() {
    let p = start_with(EventAclMode::Off).await;
    let agent = p.mint_token(TENANT, Role::Agent);
    let r = call(
        &p,
        &agent,
        "list_lineage",
        json!({ "root_event_id": "nope" }),
    )
    .await;
    assert!(result(&r)["nodes"].as_array().unwrap().is_empty(), "{r}");
    let r = call(&p, &agent, "list_lineage", json!({})).await;
    assert_eq!(r["error"]["code"], -32602, "root_event_id is required: {r}");
}
