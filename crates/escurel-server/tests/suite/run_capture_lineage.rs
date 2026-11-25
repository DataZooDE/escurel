//! A capture made inside a run (a per-run token) carries the run's lineage and requester, whatever the agent wrote:
//! the cascade's depth, path and instance path come from the run's `run-started` (filed by its runner), and the
//! person who asked for the run rides along on every capture path (`capture_event`, `start_operation`, `/ingest`).

use escurel_test_support::{AuthMode, EscurelProcess, FixtureBuilder, Opts, Role};
use serde_json::{Value, json};

const TENANT: &str = "stuttgart-ai";
const RUNNER: &str = "escurel-runner";
const PERSON: &str = "google:jr";
const NOTE_SKILL: &str = "---\nkind: skill\nid: note\ndescription: A note.\n---\n# note\n";
const WF_SKILL: &str = "---\nkind: skill\nid: deep-research\ndescription: A plan.\n\
backend: {kind: workflow}\nrun_skill: workflow-run\n\
phases: [{id: scope, produces: note, fan_out: 1}]\n---\n# deep-research\n";
const RUN_SKILL: &str = "---\nkind: skill\nid: workflow-run\n\
visibility: owner\nowner_field: requested_by\n\
optional_frontmatter: [wf_skill, status, requested_by, requester_groups, idempotency_key, conversation_ref, channel_tenant]\n\
---\n# workflow-run\n";

async fn start() -> EscurelProcess {
    EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(
            FixtureBuilder::new()
                .tenant(TENANT)
                .skill("note", NOTE_SKILL)
                .skill("deep-research", WF_SKILL)
                .skill("workflow-run", RUN_SKILL)
                .done(),
        ),
        ..Default::default()
    })
    .await
}

async fn rpc(p: &EscurelProcess, token: &str, tool: &str, args: Value) -> Value {
    reqwest::Client::new()
        .post(p.mcp_url())
        .header("authorization", format!("Bearer {token}"))
        .json(&json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                       "params": { "name": tool, "arguments": args } }))
        .send()
        .await
        .expect("post")
        .json()
        .await
        .expect("json")
}

async fn call(p: &EscurelProcess, token: &str, tool: &str, args: Value) -> Value {
    let body = rpc(p, token, tool, args).await;
    assert!(body.get("error").is_none(), "{tool}: {body}");
    body["result"]["structuredContent"].clone()
}

async fn event(p: &EscurelProcess, id: &str) -> Value {
    let admin = p.mint_token(TENANT, Role::Admin);
    call(
        p,
        &admin,
        "list_events",
        json!({ "event_id": id, "include_system": true }),
    )
    .await["events"][0]
        .clone()
}

/// A person's request, and the runner's `run-started` for the run it triggered (depth 2 into a cascade).
async fn a_run(p: &EscurelProcess, root: &str, run: &str) {
    let person = p.mint_token_with_sub(TENANT, Role::Agent, PERSON);
    call(
        p,
        &person,
        "capture_event",
        json!({ "event_id": root, "label_skill": "note", "title": "ask" }),
    )
    .await;
    let runner = p.mint_token_with_sub(TENANT, Role::Admin, RUNNER);
    call(
        p,
        &runner,
        "capture_event",
        json!({ "event_id": format!("run:{run}:started"), "kind": "system", "source": "escurel-runner",
                "mime": "application/json", "label_skill": "escurel:run", "title": "run-started", "body": "{}",
                "provenance": { "runner": { "run_id": run, "root_event_id": root, "event_id": "HOP2",
                    "depth": 2, "lineage_path": [root, "HOP1", "HOP2"],
                    "instance_path": ["markdown/instances/note/a.md", "markdown/instances/note/b.md"],
                    "target_page_id": "markdown/instances/note/c.md", "trace_id": "0123456789abcdef0123456789abcdef" } } }),
    )
    .await;
}

#[tokio::test]
async fn an_agents_capture_takes_the_runs_lineage_not_its_own() {
    let p = start().await;
    let (root, run) = ("01JRLINEAGEROOT00000000001", "run-lin");
    a_run(&p, root, run).await;
    let agent = p.mint_token_for_run(TENANT, Role::Admin, "agent:note", run, root);
    // A forged block: depth 0 and an empty instance path would reset the depth cap and the cycle check.
    call(
        &p,
        &agent,
        "capture_event",
        json!({ "event_id": "HOP3", "label_skill": "note", "title": "next",
                "provenance": { "runner": { "root_event_id": "elsewhere", "depth": 0, "instance_path": [],
                                            "lineage_path": [] } } }),
    )
    .await;
    let forged = event(&p, "HOP3").await;
    let block = &forged["provenance"]["runner"];
    assert_eq!(block["root_event_id"], json!(root), "{forged}");
    assert_eq!(block["depth"], json!(3), "{forged}");
    assert_eq!(
        block["lineage_path"],
        json!([root, "HOP1", "HOP2", "HOP3"]),
        "{forged}"
    );
    // The run's instance path plus the instance the run works on (its target), as a runner hop records it.
    assert_eq!(
        block["instance_path"],
        json!([
            "markdown/instances/note/a.md",
            "markdown/instances/note/b.md",
            "markdown/instances/note/c.md"
        ]),
        "{forged}"
    );
    assert_eq!(block["parent_run_id"], json!(run), "{forged}");
    assert_eq!(block["parent_event_id"], json!("HOP2"), "{forged}");
    assert_eq!(
        block["trace_id"],
        json!("0123456789abcdef0123456789abcdef"),
        "{forged}"
    );
    assert_eq!(
        forged["provenance"]["run_requester"],
        json!(PERSON),
        "{forged}"
    );
    // An omitted block is written the same way, so the capture stays in the cascade.
    call(
        &p,
        &agent,
        "capture_event",
        json!({ "event_id": "HOP3B", "label_skill": "note", "title": "next" }),
    )
    .await;
    let omitted = event(&p, "HOP3B").await;
    assert_eq!(
        omitted["provenance"]["runner"]["depth"],
        json!(3),
        "{omitted}"
    );
    assert_eq!(omitted["root_event_id"], json!(root), "{omitted}");
    // A block (or provenance) that is not an object is replaced, never skipped.
    for (id, provenance) in [
        ("HOP3C", json!({ "runner": null })),
        ("HOP3D", json!({ "runner": "x" })),
        ("HOP3E", json!({ "runner": [] })),
        ("HOP3F", json!([])),
    ] {
        call(
            &p,
            &agent,
            "capture_event",
            json!({ "event_id": id, "label_skill": "note", "title": "next",
                                                   "provenance": provenance }),
        )
        .await;
        let stored = event(&p, id).await;
        assert_eq!(
            stored["provenance"]["runner"]["depth"],
            json!(3),
            "{id}: {stored}"
        );
        assert_eq!(
            stored["provenance"]["runner"]["root_event_id"],
            json!(root),
            "{id}: {stored}"
        );
    }
    // Without an event id, one is minted now, so the path ends with the stored event's id.
    let minted = call(
        &p,
        &agent,
        "capture_event",
        json!({ "label_skill": "note", "title": "next" }),
    )
    .await;
    let id = minted["event_id"].as_str().expect("event id").to_owned();
    let stored = event(&p, &id).await;
    assert_eq!(
        stored["provenance"]["runner"]["lineage_path"][3],
        json!(id),
        "{stored}"
    );
    // The capture's own target is NOT added (admission would read it as a cycle): a fresh target stays admissible.
    call(&p, &agent, "capture_event", json!({ "event_id": "HOP3G", "label_skill": "note", "title": "next",
                                               "instance_page_id": "markdown/instances/note/d.md" })).await;
    let on_page = event(&p, "HOP3G").await;
    assert_eq!(
        on_page["provenance"]["runner"]["instance_path"],
        json!([
            "markdown/instances/note/a.md",
            "markdown/instances/note/b.md",
            "markdown/instances/note/c.md"
        ]),
        "{on_page}"
    );
}

#[tokio::test]
async fn a_start_someone_else_filed_does_not_set_the_lineage() {
    let p = start().await;
    let (root, run) = ("01JRLINEAGEROOT00000000002", "run-other");
    let person = p.mint_token_with_sub(TENANT, Role::Agent, PERSON);
    call(
        &p,
        &person,
        "capture_event",
        json!({ "event_id": root, "label_skill": "note", "title": "ask" }),
    )
    .await;
    // A `run-started` filed by someone who is not the token's runner says nothing about the run.
    let admin = p.mint_token_with_sub(TENANT, Role::Admin, "admin:mallory");
    call(
        &p,
        &admin,
        "capture_event",
        json!({ "event_id": format!("run:{run}:started"), "kind": "system", "source": "x", "mime": "application/json",
                "label_skill": "escurel:run", "title": "run-started", "body": "{}",
                "provenance": { "runner": { "run_id": run, "root_event_id": root, "event_id": root, "depth": 0,
                                            "lineage_path": [root] } } }),
    )
    .await;
    let agent = p.mint_token_for_run(TENANT, Role::Admin, "agent:note", run, root);
    call(
        &p,
        &agent,
        "capture_event",
        json!({ "event_id": "HOPX", "label_skill": "note", "title": "next" }),
    )
    .await;
    let hop = event(&p, "HOPX").await;
    // Only the token's root (and a parseable depth): the forged start's lineage is not taken.
    assert_eq!(
        hop["provenance"]["runner"]["root_event_id"],
        json!(root),
        "{hop}"
    );
    assert_eq!(hop["provenance"]["runner"]["depth"], json!(0), "{hop}");
    assert!(
        hop["provenance"]["runner"].get("lineage_path").is_none(),
        "{hop}"
    );
    assert_eq!(
        hop["root_event_id"],
        json!(root),
        "the root is still the token's: {hop}"
    );
}

#[tokio::test]
async fn a_workflow_or_an_upload_a_run_starts_carries_the_requester() {
    let p = start().await;
    let (root, run) = ("01JRLINEAGEROOT00000000003", "run-wf");
    a_run(&p, root, run).await;
    let agent = p.mint_token_for_run(TENANT, Role::Admin, "agent:note", run, root);
    let started = call(
        &p,
        &agent,
        "start_operation",
        json!({ "wf_skill": "deep-research" }),
    )
    .await;
    let admin = p.mint_token(TENANT, Role::Admin);
    let invocations = call(
        &p,
        &admin,
        "list_events",
        json!({ "label_skill": "deep-research", "include_system": true }),
    )
    .await;
    let invocation = invocations["events"]
        .as_array()
        .expect("events")
        .iter()
        .find(|e| e["provenance"]["workflow"]["phase"] == json!("invoke"))
        .cloned()
        .unwrap_or_else(|| panic!("no invocation: {started} {invocations}"));
    assert_eq!(
        invocation["provenance"]["run_requester"],
        json!(PERSON),
        "{invocation}"
    );
    assert_eq!(
        invocation["provenance"]["runner"]["depth"],
        json!(3),
        "{invocation}"
    );
    assert_eq!(invocation["root_event_id"], json!(root), "{invocation}");

    let uploaded = reqwest::Client::new()
        .post(format!("{}/ingest/upload", p.base_url()))
        .header("authorization", format!("Bearer {agent}"))
        .json(&json!({ "content_type": "text/plain", "bytes_b64": "aGVsbG8=", "event_id": "01JRUPLOADINRUN00000000001",
                       "title": "upload" }))
        .send()
        .await
        .expect("post");
    let status = uploaded.status();
    let body: Value = uploaded.json().await.unwrap_or(Value::Null);
    assert!(status.is_success(), "{status}: {body}");
    let upload = event(&p, "01JRUPLOADINRUN00000000001").await;
    assert_eq!(
        upload["provenance"]["run_requester"],
        json!(PERSON),
        "{upload}"
    );
    assert_eq!(
        upload["provenance"]["runner"]["depth"],
        json!(3),
        "{upload}"
    );
    assert_eq!(
        upload["provenance"]["captured_via"],
        json!(RUNNER),
        "the runner trusts it: {upload}"
    );
    assert_eq!(upload["root_event_id"], json!(root), "{upload}");
}

#[tokio::test]
async fn a_run_with_no_start_keeps_only_the_tokens_root() {
    let p = start().await;
    let root = "01JRLINEAGEROOT00000000004";
    let person = p.mint_token_with_sub(TENANT, Role::Agent, PERSON);
    call(
        &p,
        &person,
        "capture_event",
        json!({ "event_id": root, "label_skill": "note", "title": "ask" }),
    )
    .await;
    // No `run-started` (a runner that could not file it): the root is still the token's (the per-root budget
    // holds); depth and paths are not the gateway's to know (a documented limit).
    let agent = p.mint_token_for_run(TENANT, Role::Admin, "agent:note", "run-nostart", root);
    call(
        &p,
        &agent,
        "capture_event",
        json!({ "event_id": "HOPN", "label_skill": "note", "title": "next",
                "provenance": { "runner": { "root_event_id": "elsewhere", "depth": 1 } } }),
    )
    .await;
    let hop = event(&p, "HOPN").await;
    assert_eq!(
        hop["provenance"]["runner"]["root_event_id"],
        json!(root),
        "{hop}"
    );
    assert_eq!(hop["root_event_id"], json!(root), "{hop}");
    // Whatever shape the agent sends, the block carries the token's root and a numeric depth, on every path.
    for (id, provenance) in [
        ("HOPN2", Value::Null),
        ("HOPN3", json!({ "runner": null })),
        ("HOPN4", json!({ "runner": "x" })),
        ("HOPN5", json!({ "runner": [] })),
        (
            "HOPN6",
            json!({ "runner": { "root_event_id": "elsewhere" } }),
        ),
    ] {
        let mut args = json!({ "event_id": id, "label_skill": "note", "title": "next" });
        if !provenance.is_null() {
            args["provenance"] = provenance;
        }
        call(&p, &agent, "capture_event", args).await;
        let stored = event(&p, id).await;
        assert_eq!(
            stored["provenance"]["runner"]["root_event_id"],
            json!(root),
            "{id}: {stored}"
        );
        assert!(
            stored["provenance"]["runner"]["depth"].is_u64(),
            "{id}: {stored}"
        );
    }
    call(
        &p,
        &agent,
        "start_operation",
        json!({ "wf_skill": "deep-research" }),
    )
    .await;
    let admin = p.mint_token(TENANT, Role::Admin);
    let invocations = call(
        &p,
        &admin,
        "list_events",
        json!({ "label_skill": "deep-research", "include_system": true }),
    )
    .await;
    let invocation = invocations["events"][0].clone();
    assert_eq!(
        invocation["provenance"]["runner"]["root_event_id"],
        json!(root),
        "{invocation}"
    );
    assert!(
        invocation["provenance"]["runner"]["depth"].is_u64(),
        "{invocation}"
    );
}
