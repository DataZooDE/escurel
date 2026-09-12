//! DoD test for PR-4 of the dynamic-workflows program — the **dispatch-loop
//! branch**, with **no mocks**.
//!
//! Against a real `EscurelProcess` gateway with a real `kind: workflow` plan
//! (`deep-research`: scope → synthesize, both width-1) and the real runner
//! (real echo-harness), `capture_event` a workflow **invocation** and assert
//! — via the real `/mcp` `list_instances` surface — that the reducer drove
//! the plan end to end: the scope phase produced a `research-angle` instance,
//! then the synthesize phase produced a `research-report` instance, each
//! run-scoped by the deterministic pre-flagged page id (`§3.6`). The whole
//! chain rides the same poll → trigger → package → harness → reconcile
//! pipeline as a cascade; the only change is the dispatch loop calling the
//! reducer instead of `emit_cascade` for a workflow-labelled trigger.

use std::net::TcpListener;
use std::process::{Child, Command};
use std::time::{Duration, Instant};

use escurel_test_support::{AuthMode, EscurelProcess, FixtureBuilder, Opts, Role};
use serde_json::{Value, json};

const TENANT: &str = "acme";

const WF_SKILL: &str = "deep-research";
// A real two-phase workflow plan: scope (produces one research-angle) then
// synthesize (produces one research-report). Inline-flow YAML for the
// `backend`/`phases` blocks avoids block-indent pitfalls; the reducer reads
// this frontmatter via `expand`.
const WF_SKILL_BODY: &str = "---\n\
type: skill\n\
id: deep-research\n\
description: Two-phase workflow test plan.\n\
backend: {kind: workflow}\n\
run_skill: workflow-run\n\
phases: [{id: scope, produces: research-angle, fan_out: 1}, {id: synthesize, produces: research-report, fan_out: 1}]\n\
---\n\
# deep-research\n\nFan out, then synthesize.\n";

// A one-phase plan whose step declares `harness: delegate` — a harness this
// runner cannot build. Used to prove the step FAILS CLOSED (dead-letters) rather
// than silently running the default echo harness and fabricating a `succeeded`
// produced instance (async-ops Phase 4 slice 3a; crew F8).
const DELEGATE_WF_SKILL: &str = "delegate-plan";
const DELEGATE_WF_BODY: &str = "---\n\
type: skill\n\
id: delegate-plan\n\
description: A one-phase plan whose step delegates to an unavailable harness.\n\
backend: {kind: workflow}\n\
run_skill: workflow-run\n\
phases: [{id: produce, produces: research-report, fan_out: 1, harness: delegate}]\n\
---\n\
# delegate-plan\n\nDelegate the one step to the agent.\n";

// A plan whose first phase alone projects 10 runs — used to prove the
// up-front budget gate refuses to start it when max_runs_per_root is small.
const BIG_WF_SKILL: &str = "over-budget";
const BIG_WF_SKILL_BODY: &str = "---\n\
type: skill\n\
id: over-budget\n\
description: A plan too large for a tiny budget.\n\
backend: {kind: workflow}\n\
run_skill: workflow-run\n\
phases: [{id: scope, produces: research-angle, fan_out: 10}]\n\
---\n\
# over-budget\n\nToo big.\n";

// A three-phase BARRIER plan: extract one `claims` set, fan a width-3
// adversarial `verify` barrier over it (three skeptics), then `synthesize` a
// report once the barrier closes. This is the shape the linear scope→synthesize
// plan cannot exercise — it forces the reducer's quorum tally and the harness's
// per-skeptic `vote_index` stamping.
const VERIFY_WF_SKILL: &str = "claim-check";
const VERIFY_WF_BODY: &str = "---\n\
type: skill\n\
id: claim-check\n\
description: Barrier workflow test plan — extract, adversarially verify, synthesize.\n\
backend: {kind: workflow}\n\
run_skill: workflow-run\n\
phases: [{id: extract, produces: claims, fan_out: 1}, {id: verify, produces: verify-vote, fan_out: {over: claims, width: verify.votes_per_claim}, max_targets: 1}, {id: synthesize, produces: research-report, fan_out: 1}]\n\
verify: {votes_per_claim: 3, refutations_required: 2}\n\
---\n\
# claim-check\n\nExtract claims, verify them adversarially, synthesize.\n";

// Per-phase framing rides the `produces:` skill body (the packager's
// `instructions`), not the plan's sections.
const CLAIMS_SKILL_BODY: &str = "---\ntype: skill\nid: claims\n---\n# claims\n\n\
Read the question on the run board and extract 2-4 concise, checkable factual \
claims that answer it. Write them as a short numbered list.\n";
const VERIFY_VOTE_SKILL_BODY: &str = "---\ntype: skill\nid: verify-vote\n\
required_frontmatter: [claim, vote_index, verdict]\n\
optional_frontmatter: [reason, workflow_run]\n---\n# verify-vote\n\n\
You are an adversarial skeptic. Try to refute the claims under review; if they \
hold up, vote valid. Be rigorous and cite your reasoning in one line.\n";

const ANGLE_SKILL_BODY: &str =
    "---\ntype: skill\nid: research-angle\n---\n# research-angle\n\nOne search angle.\n";
const REPORT_SKILL_BODY: &str =
    "---\ntype: skill\nid: research-report\n---\n# research-report\n\nThe cited report.\n";
// Owner-scoped (crew Phase-2 F3): a `start_operation` board carries
// `requested_by` and is readable only by that requester (+admin). Boards created
// directly via `capture_event` in the reducer-focused tests carry no
// `requested_by`, so their `get_operation` reads use an admin token.
const RUN_SKILL_BODY: &str = "---\ntype: skill\nid: workflow-run\n\
visibility: owner\nowner_field: requested_by\n\
optional_frontmatter: [wf_skill, status, requested_by, requester_groups, idempotency_key, conversation_ref, channel_tenant]\n\
---\n# workflow-run\n\nThe run board.\n";

struct ChildGuard(Child);
impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .expect("bind ephemeral port")
        .local_addr()
        .expect("read local_addr")
        .port()
}

/// A stub channel courier: an in-process `/v1/outbound` sink that records every
/// terminal-delivery POST (async-ops Phase 3). Returns its URL and the shared
/// buffer of received bodies.
async fn spawn_outbound_sink() -> (String, std::sync::Arc<std::sync::Mutex<Vec<Value>>>) {
    use axum::extract::State;
    use axum::routing::post;
    use axum::{Json, Router};

    type Buf = std::sync::Arc<std::sync::Mutex<Vec<Value>>>;
    let received: Buf = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));

    async fn handler(State(buf): State<Buf>, Json(body): Json<Value>) -> axum::http::StatusCode {
        buf.lock().expect("sink mutex").push(body);
        axum::http::StatusCode::OK
    }

    let app = Router::new()
        .route("/v1/outbound", post(handler))
        .with_state(received.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind sink");
    let addr = listener.local_addr().expect("sink addr");
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (format!("http://{addr}/v1/outbound"), received)
}

/// A stub channel courier that REQUIRES a server-to-server bearer, exactly as the
/// agent's delivery receiver does (`AGENT_ASYNC_CALLBACK_BEARER`): a POST with a
/// missing or wrong `Authorization: Bearer` is refused 401 and NOT recorded, so a
/// runner that fails to present the configured bearer delivers nothing. Returns
/// the sink URL and the buffer of ACCEPTED bodies.
async fn spawn_outbound_sink_requiring_bearer(
    want: &str,
) -> (String, std::sync::Arc<std::sync::Mutex<Vec<Value>>>) {
    use axum::extract::State;
    use axum::http::HeaderMap;
    use axum::routing::post;
    use axum::{Json, Router};

    type Buf = std::sync::Arc<std::sync::Mutex<Vec<Value>>>;
    let received: Buf = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let want = want.to_owned();

    async fn handler(
        State((buf, want)): State<(Buf, String)>,
        headers: HeaderMap,
        Json(body): Json<Value>,
    ) -> axum::http::StatusCode {
        let presented = headers
            .get(axum::http::header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .unwrap_or_default();
        if presented != want {
            return axum::http::StatusCode::UNAUTHORIZED;
        }
        buf.lock().expect("sink mutex").push(body);
        axum::http::StatusCode::OK
    }

    let app = Router::new()
        .route("/v1/outbound", post(handler))
        .with_state((received.clone(), want));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind sink");
    let addr = listener.local_addr().expect("sink addr");
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (format!("http://{addr}/v1/outbound"), received)
}

async fn call_mcp(p: &EscurelProcess, role: Role, name: &str, args: Value) -> Value {
    let token = p.mint_token(TENANT, role);
    let resp = reqwest::Client::new()
        .post(p.mcp_url())
        .header("authorization", format!("Bearer {token}"))
        .json(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": { "name": name, "arguments": args },
        }))
        .send()
        .await
        .expect("post /mcp");
    assert_eq!(resp.status(), 200, "http status");
    let body: Value = resp.json().await.unwrap();
    assert!(body.get("error").is_none(), "tool {name} error: {body}");
    let result = body["result"].clone();
    result.get("structuredContent").cloned().unwrap_or(result)
}

/// Like [`call_mcp`] but signs the token with an explicit `subject` — for
/// cross-caller ACL tests (caller B reading caller A's owner-scoped operation).
async fn call_mcp_as(
    p: &EscurelProcess,
    role: Role,
    subject: &str,
    name: &str,
    args: Value,
) -> Value {
    let token = p.mint_token_with_sub(TENANT, role, subject);
    let resp = reqwest::Client::new()
        .post(p.mcp_url())
        .header("authorization", format!("Bearer {token}"))
        .json(&json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": { "name": name, "arguments": args },
        }))
        .send()
        .await
        .expect("post /mcp");
    let body: Value = resp.json().await.unwrap();
    assert!(body.get("error").is_none(), "tool {name} error: {body}");
    let result = body["result"].clone();
    result.get("structuredContent").cloned().unwrap_or(result)
}

/// Like [`call_mcp`] but asserts the tool returned a JSON-RPC ERROR, and returns
/// the error object. Used for negative security cases.
async fn call_mcp_err(p: &EscurelProcess, role: Role, name: &str, args: Value) -> Value {
    let token = p.mint_token(TENANT, role);
    let resp = reqwest::Client::new()
        .post(p.mcp_url())
        .header("authorization", format!("Bearer {token}"))
        .json(&json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": { "name": name, "arguments": args },
        }))
        .send()
        .await
        .expect("post /mcp");
    let body: Value = resp.json().await.unwrap();
    assert!(
        body.get("error").is_some(),
        "expected {name} to error, got: {body}"
    );
    body["error"].clone()
}

/// Poll `list_instances(skill)` until an instance whose page id starts with
/// `prefix` appears, or the deadline passes (returns its page id).
async fn await_instance(p: &EscurelProcess, skill: &str, prefix: &str, secs: u64) -> String {
    let deadline = Instant::now() + Duration::from_secs(secs);
    loop {
        let r = call_mcp(
            p,
            Role::Agent,
            "list_instances",
            json!({ "skill_id": skill }),
        )
        .await;
        if let Some(page) = r["instances"].as_array().and_then(|is| {
            is.iter()
                .filter_map(|i| i["page_id"].as_str())
                .find(|pid| pid.starts_with(prefix))
        }) {
            return page.to_owned();
        }
        if Instant::now() >= deadline {
            panic!("no {skill} instance with prefix {prefix} appeared within {secs}s");
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

/// Wait until the spawned runner answers `GET /healthz` — which it binds only
/// AFTER startup `recover_workflows` has run. Creating the board + invocation
/// only after this guarantees startup recovery scanned an EMPTY corpus, so the
/// invocation has a single driver (the poller), exactly as in production where
/// the long-running runner never sees a fresh board at boot. Without it, a
/// slow-booting runner (under parallel test load) can scan the board mid-flight
/// and race the invocation.
async fn wait_for_runner_ready(listen: &str) {
    let url = format!("http://{listen}/healthz");
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if reqwest::Client::new()
            .get(&url)
            .send()
            .await
            .map(|r| r.status().is_success())
            .unwrap_or(false)
        {
            return;
        }
        if Instant::now() >= deadline {
            panic!("runner did not become ready (healthz) within 20s");
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// Materialise the run board a `start_operation` would have created before its
/// invocation event. These reducer/barrier tests inject the invocation as a raw
/// admin `capture_event` (to keep a fixed run-board id), but the invocation no
/// longer folds the board through the harness — the reducer owns the board and
/// the runner drives it directly — so a test that later reads the board via
/// `get_operation` must create the page itself, exactly as the facade does.
async fn create_run_board(p: &EscurelProcess, run_page: &str, wf_skill: &str) {
    let slug = run_page
        .strip_prefix("markdown/instances/workflow-run/")
        .and_then(|s| s.strip_suffix(".md"))
        .expect("run board page id shape");
    let content = format!(
        "---\ntype: instance\nskill: workflow-run\nid: {slug}\nwf_skill: {wf_skill}\n\
         ---\n# operation\n\nAsync operation run board.\n"
    );
    let written = call_mcp(
        p,
        Role::Admin,
        "update_page",
        json!({ "page_id": run_page, "content": content }),
    )
    .await;
    assert_eq!(
        written["ok"],
        json!(true),
        "run board must write cleanly: {written}"
    );
}

#[tokio::test]
async fn workflow_invocation_drives_scope_then_synthesize_to_completion() {
    // 1. Real gateway with the workflow plan + its two produced skills + the
    //    run-board skill.
    let gateway = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(
            FixtureBuilder::new()
                .tenant(TENANT)
                .skill(WF_SKILL, WF_SKILL_BODY)
                .skill("research-angle", ANGLE_SKILL_BODY)
                .skill("research-report", REPORT_SKILL_BODY)
                .skill("workflow-run", RUN_SKILL_BODY)
                .done(),
        ),
        ..Default::default()
    })
    .await;

    // 2. Capture the workflow INVOCATION: label the plan skill, pre-flag the
    //    run-board instance, and carry a `provenance.workflow` block so the
    //    dispatch loop routes the invocation to the reducer.
    let run_page = "markdown/instances/workflow-run/r1.md";
    // The facade creates the board before the invocation; mirror that here (the
    // invocation no longer folds it via the harness).
    create_run_board(&gateway, run_page, WF_SKILL).await;
    call_mcp(
        &gateway,
        // A workflow invocation carries `provenance.workflow`, which the gateway
        // now accepts only from an admin/system identity (async-ops 2c-ii — a
        // non-admin caller starts a workflow via `start_operation`, not a raw
        // capture_event). These reducer/barrier tests inject the invocation as
        // that system identity to keep a fixed run-board id for their prefix
        // assertions; the facade path is covered by the start_operation tests.
        Role::Admin,
        "capture_event",
        json!({
            "source": "manual",
            "mime": "text/plain",
            "label_skill": WF_SKILL,
            "instance_page_id": run_page,
            "title": "invoke deep-research",
            "body": "Answer the research question.",
            "provenance": {
                "workflow": { "run": run_page, "wf_skill": WF_SKILL, "phase": "invoke" }
            }
        }),
    )
    .await;

    // 3. Spawn the real runner (echo harness), generous loop limits, fast poll.
    // The runner authenticates as an admin identity (in production it mints its
    // own `escurel:admin` bearer): its orchestration writes include the reserved
    // `escurel:run-status` status events, which the capture guard admits only for
    // admin. (The per-run HARNESS token is the separate caller-scoped one, 2c.)
    let token = gateway.mint_token(TENANT, Role::Admin);
    let port = free_port();
    let listen = format!("127.0.0.1:{port}");
    let ledger_dir = tempfile::tempdir().expect("tempdir for ledger");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_escurel-runner"));
    cmd.env("ESCUREL_RUNNER_LISTEN", &listen)
        .env("ESCUREL_RUNNER_GATEWAY_URL", gateway.base_url())
        .env("ESCUREL_RUNNER_TENANT", TENANT)
        .env("ESCUREL_RUNNER_TOKEN", &token)
        .env("ESCUREL_RUNNER_HARNESS", "echo")
        .env(
            "ESCUREL_RUNNER_LEDGER_PATH",
            ledger_dir.path().join("ledger.sqlite").to_str().unwrap(),
        )
        .env("ESCUREL_RUNNER_MAX_DEPTH", "16")
        .env("ESCUREL_RUNNER_MAX_RUNS_PER_ROOT", "64")
        .env("ESCUREL_RUNNER_MAX_ATTEMPTS", "3")
        .env("ESCUREL_RUNNER_RETRY_BACKOFF", "100ms")
        .env("ESCUREL_RUNNER_POLL_INTERVAL", "250ms");
    let _runner = ChildGuard(cmd.spawn().expect("spawn escurel-runner"));

    // 4. Phase A: the reducer emits a scope step whose echo run creates the
    //    run-scoped research-angle instance at its deterministic pre-flagged id.
    let angle = await_instance(
        &gateway,
        "research-angle",
        "markdown/instances/research-angle/r1-scope-",
        45,
    )
    .await;
    assert!(angle.ends_with(".md"), "angle page id: {angle}");

    // 5. Phase B: once scope is complete, the reducer advances to synthesize,
    //    whose echo run creates the research-report instance — proving the
    //    dispatch loop drove reduce → emit → process → reduce → emit → done.
    let report = await_instance(
        &gateway,
        "research-report",
        "markdown/instances/research-report/r1-synthesize-",
        45,
    )
    .await;
    assert!(report.ends_with(".md"), "report page id: {report}");

    // Operation status (async-ops Phase 0.2), event-sourced: once every phase is
    // complete the runner records a terminal `succeeded` status as an assigned
    // event on the run board — the record `get_operation` will read. Poll the
    // board's event history for it.
    let run_page = "markdown/instances/workflow-run/r1.md";
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    let succeeded = loop {
        let events = call_mcp(
            &gateway,
            Role::Agent,
            "list_events",
            json!({ "instance_page_id": run_page }),
        )
        .await;
        let found = events["events"].as_array().is_some_and(|es| {
            es.iter().any(|e| {
                e["title"].as_str() == Some("status: succeeded")
                    || e["provenance"]["run_status"].as_str() == Some("succeeded")
            })
        });
        if found {
            break true;
        }
        if std::time::Instant::now() >= deadline {
            break false;
        }
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    };
    assert!(
        succeeded,
        "the operation must reach a terminal `succeeded` status event on {run_page}"
    );

    // async-ops Phase 2: the `get_operation` facade derives that terminal state
    // from the append-only status events (by precedence), so a caller polls one
    // read instead of scanning the event log. This board was created directly
    // via capture_event (no `requested_by`), so an admin token reads it; the
    // owner-scoped facade path is covered by the start_operation tests below.
    let op = call_mcp(
        &gateway,
        Role::Admin,
        "get_operation",
        json!({ "operation_id": run_page }),
    )
    .await;
    assert_eq!(
        op["found"],
        json!(true),
        "get_operation found the run board"
    );
    assert_eq!(
        op["status"],
        json!("succeeded"),
        "get_operation derives `succeeded` for a completed workflow: {op}"
    );

    // A bogus operation id is `found: false` — the same shape a cross-caller
    // denial returns, so existence never leaks.
    let missing = call_mcp(
        &gateway,
        Role::Agent,
        "get_operation",
        json!({ "operation_id": "markdown/instances/workflow-run/does-not-exist.md" }),
    )
    .await;
    assert_eq!(
        missing["found"],
        json!(false),
        "get_operation on an unknown id is not-found, not an error: {missing}"
    );
}

/// Seed a succeeded `run-status` event carrying `provenance` onto `run_page`, as
/// admin (the reserved label is admin-only), and mark it processed so
/// `latest_labeled_event` sees it. Returns nothing; the caller then reads the
/// board with `get_operation`.
async fn seed_succeeded_status(p: &EscurelProcess, run_page: &str, provenance: Value) {
    let ev = call_mcp(
        p,
        Role::Admin,
        "capture_event",
        json!({
            "source": "manual",
            "mime": "text/plain",
            "label_skill": escurel_types::OPERATION_STATUS_LABEL,
            "instance_page_id": run_page,
            "title": "status: succeeded",
            "body": "",
            "provenance": provenance,
        }),
    )
    .await;
    let event_id = ev["event_id"].as_str().expect("capture returns event_id");
    call_mcp(
        p,
        Role::Admin,
        "assign_event",
        json!({ "event_id": event_id, "instance_page_id": run_page }),
    )
    .await;
}

/// async-ops Phase 4 (slice 1): `get_operation` surfaces the operation's
/// `result_ref` when the terminal status event carries one — the read-side the
/// agent's `GetOperationTool` already promises. A well-formed `ResultRef` is
/// returned; a malformed one is omitted (fail-safe — never leak an unvalidated
/// ref). The producing side (a harness stamping it) is slice 2.
#[tokio::test]
async fn get_operation_surfaces_a_result_ref_from_the_terminal_status_event() {
    let gateway = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(
            FixtureBuilder::new()
                .tenant(TENANT)
                .skill("workflow-run", RUN_SKILL_BODY)
                .done(),
        ),
        ..Default::default()
    })
    .await;

    // A completed operation whose terminal status carries a valid ScenarioParquet
    // result_ref.
    let good = "markdown/instances/workflow-run/op-with-result.md";
    create_run_board(&gateway, good, "scenario").await;
    seed_succeeded_status(
        &gateway,
        good,
        json!({
            "run_status": "succeeded",
            "result_ref": { "kind": "scenario_parquet", "scenario_id": "scn-demo-01" }
        }),
    )
    .await;
    let op = call_mcp(
        &gateway,
        Role::Admin,
        "get_operation",
        json!({ "operation_id": good }),
    )
    .await;
    assert_eq!(op["found"], json!(true), "{op}");
    assert_eq!(op["status"], json!("succeeded"), "{op}");
    assert_eq!(
        op["result_ref"],
        json!({ "kind": "scenario_parquet", "scenario_id": "scn-demo-01" }),
        "get_operation must surface the terminal result_ref verbatim: {op}"
    );

    // A malformed result_ref (unknown kind) is dropped — the status still reads,
    // but no unvalidated ref reaches the caller.
    let bad = "markdown/instances/workflow-run/op-bad-result.md";
    create_run_board(&gateway, bad, "scenario").await;
    seed_succeeded_status(
        &gateway,
        bad,
        json!({
            "run_status": "succeeded",
            "result_ref": { "kind": "totally-not-a-real-kind", "x": 1 }
        }),
    )
    .await;
    let op = call_mcp(
        &gateway,
        Role::Admin,
        "get_operation",
        json!({ "operation_id": bad }),
    )
    .await;
    assert_eq!(op["status"], json!("succeeded"), "{op}");
    assert!(
        op.get("result_ref").is_none(),
        "a malformed result_ref must be omitted, not passed through: {op}"
    );
}

/// async-ops Phase 4 (slice 2): a PRODUCING harness's `result_ref` rides its
/// `HarnessOutcome` → the confirmed effect → the terminal `succeeded` status
/// event, and `get_operation` surfaces it. Exercised end-to-end with the echo
/// harness's `ESCUREL_ECHO_RESULT_REF` knob standing in for the (slice-3)
/// scenario producer: a real gateway + runner drive the plan to completion and
/// the produced ref comes back out of `get_operation`.
#[tokio::test]
async fn a_harness_produced_result_ref_reaches_get_operation() {
    let gateway = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(
            FixtureBuilder::new()
                .tenant(TENANT)
                .skill(WF_SKILL, WF_SKILL_BODY)
                .skill("research-angle", ANGLE_SKILL_BODY)
                .skill("research-report", REPORT_SKILL_BODY)
                .skill("workflow-run", RUN_SKILL_BODY)
                .done(),
        ),
        ..Default::default()
    })
    .await;

    let run_page = "markdown/instances/workflow-run/r-resultref.md";
    create_run_board(&gateway, run_page, WF_SKILL).await;
    call_mcp(
        &gateway,
        Role::Admin,
        "capture_event",
        json!({
            "source": "manual",
            "mime": "text/plain",
            "label_skill": WF_SKILL,
            "instance_page_id": run_page,
            "title": "invoke",
            "body": "Answer the research question.",
            "provenance": { "workflow": { "run": run_page, "wf_skill": WF_SKILL, "phase": "invoke" } }
        }),
    )
    .await;

    // A runner whose echo harness reports a ScenarioParquet result_ref on every
    // write (the knob) — the producing side slice 2 threads through.
    let token = gateway.mint_token(TENANT, Role::Admin);
    let port = free_port();
    let ledger_dir = tempfile::tempdir().expect("tempdir for ledger");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_escurel-runner"));
    cmd.env("ESCUREL_RUNNER_LISTEN", format!("127.0.0.1:{port}"))
        .env("ESCUREL_RUNNER_GATEWAY_URL", gateway.base_url())
        .env("ESCUREL_RUNNER_TENANT", TENANT)
        .env("ESCUREL_RUNNER_TOKEN", &token)
        .env("ESCUREL_RUNNER_HARNESS", "echo")
        .env("ESCUREL_ECHO_RESULT_REF", "scn-e2e-01")
        .env(
            "ESCUREL_RUNNER_LEDGER_PATH",
            ledger_dir.path().join("ledger.sqlite").to_str().unwrap(),
        )
        .env("ESCUREL_RUNNER_MAX_DEPTH", "16")
        .env("ESCUREL_RUNNER_MAX_RUNS_PER_ROOT", "64")
        .env("ESCUREL_RUNNER_MAX_ATTEMPTS", "3")
        .env("ESCUREL_RUNNER_RETRY_BACKOFF", "100ms")
        .env("ESCUREL_RUNNER_POLL_INTERVAL", "250ms");
    let _runner = ChildGuard(cmd.spawn().expect("spawn escurel-runner"));

    assert!(
        await_operation_status(&gateway, run_page, "succeeded", 60).await,
        "the operation must drive to a terminal succeeded"
    );

    let op = call_mcp(
        &gateway,
        Role::Admin,
        "get_operation",
        json!({ "operation_id": run_page }),
    )
    .await;
    assert_eq!(op["status"], json!("succeeded"), "{op}");
    assert_eq!(
        op["result_ref"],
        json!({ "kind": "scenario_parquet", "scenario_id": "scn-e2e-01" }),
        "the harness-produced result_ref must ride through to get_operation: {op}"
    );
}

/// async-ops Phase 4 slice 3a (crew F8, a live defect): a workflow phase
/// declaring `harness: delegate` on a runner that cannot build it must FAIL
/// CLOSED — the step dead-letters and the operation reaches terminal `failed`,
/// and the default echo harness must NOT run in its place and fabricate a
/// `succeeded` produced instance. (Before the fix, `resolve_harness` silently
/// used the default, so echo wrote a real research-report and the op `succeeded`.)
#[tokio::test]
async fn a_step_with_an_unbuildable_declared_harness_dead_letters_not_runs_echo() {
    let gateway = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(
            FixtureBuilder::new()
                .tenant(TENANT)
                .skill(DELEGATE_WF_SKILL, DELEGATE_WF_BODY)
                .skill("research-report", REPORT_SKILL_BODY)
                .skill("workflow-run", RUN_SKILL_BODY)
                .done(),
        ),
        ..Default::default()
    })
    .await;

    let run_page = "markdown/instances/workflow-run/r-delegate.md";
    create_run_board(&gateway, run_page, DELEGATE_WF_SKILL).await;
    call_mcp(
        &gateway,
        Role::Admin,
        "capture_event",
        json!({
            "source": "manual",
            "mime": "text/plain",
            "label_skill": DELEGATE_WF_SKILL,
            "instance_page_id": run_page,
            "title": "invoke delegate-plan",
            "body": "Run the one delegated step.",
            "provenance": { "workflow": { "run": run_page, "wf_skill": DELEGATE_WF_SKILL, "phase": "invoke" } }
        }),
    )
    .await;

    // A runner whose ONLY harness is the default echo — it cannot build `delegate`.
    let token = gateway.mint_token(TENANT, Role::Admin);
    let port = free_port();
    let ledger_dir = tempfile::tempdir().expect("tempdir for ledger");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_escurel-runner"));
    cmd.env("ESCUREL_RUNNER_LISTEN", format!("127.0.0.1:{port}"))
        .env("ESCUREL_RUNNER_GATEWAY_URL", gateway.base_url())
        .env("ESCUREL_RUNNER_TENANT", TENANT)
        .env("ESCUREL_RUNNER_TOKEN", &token)
        .env("ESCUREL_RUNNER_HARNESS", "echo")
        .env(
            "ESCUREL_RUNNER_LEDGER_PATH",
            ledger_dir.path().join("ledger.sqlite").to_str().unwrap(),
        )
        .env("ESCUREL_RUNNER_MAX_DEPTH", "16")
        .env("ESCUREL_RUNNER_MAX_RUNS_PER_ROOT", "64")
        // Fail fast: one attempt, so the delegate step's permanent refusal
        // dead-letters promptly rather than burning a retry budget.
        .env("ESCUREL_RUNNER_MAX_ATTEMPTS", "1")
        .env("ESCUREL_RUNNER_RETRY_BACKOFF", "100ms")
        .env("ESCUREL_RUNNER_POLL_INTERVAL", "250ms");
    let _runner = ChildGuard(cmd.spawn().expect("spawn escurel-runner"));

    // The delegated step refuses (Unsupported → Permanent → dead-letter), and the
    // reducer drives the operation to a terminal `failed`.
    assert!(
        await_operation_status(&gateway, run_page, "failed", 60).await,
        "a step declaring an unbuildable harness must dead-letter the operation to `failed`, \
         not run echo and succeed"
    );

    // And echo must NOT have fabricated the produced research-report instance.
    let insts = call_mcp(
        &gateway,
        Role::Agent,
        "list_instances",
        json!({ "skill_id": "research-report" }),
    )
    .await;
    let fabricated = insts["instances"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter(|i| {
                    i["page_id"]
                        .as_str()
                        .is_some_and(|p| p.contains("r-delegate"))
                })
                .count()
        })
        .unwrap_or(0);
    assert_eq!(
        fabricated, 0,
        "the default harness must not fabricate a produced instance for the refused step: {insts}"
    );
}

/// Poll the run board's event history for an operation-status event whose
/// `provenance.run_status` matches `want`, up to `secs`. Returns whether it
/// appeared.
async fn await_operation_status(p: &EscurelProcess, run_page: &str, want: &str, secs: u64) -> bool {
    let deadline = Instant::now() + Duration::from_secs(secs);
    loop {
        let events = call_mcp(
            p,
            Role::Agent,
            "list_events",
            json!({ "instance_page_id": run_page }),
        )
        .await;
        let found = events["events"].as_array().is_some_and(|es| {
            es.iter()
                .any(|e| e["provenance"]["run_status"].as_str() == Some(want))
        });
        if found {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

/// Red/green regression for the invocation-routing fix: a workflow INVOCATION
/// must be driven by the reducer (admin), NEVER handed to the caller-scoped
/// harness to "fold" into the run board — the harness does not own the board and
/// cannot `assign_event` (`WORKFLOW_STEP_TOOLS` denies it), so folding the
/// invocation there dead-lettered EVERY live `start_operation` ("event not yet
/// processed"). The echo suite missed it because echo's requester owns the board
/// and folds it cleanly.
///
/// The discriminator: `ESCUREL_ECHO_FAIL_SKILL` fails any harness run whose
/// `label_skill` matches. Set it to the PLAN skill — the invocation event's own
/// label. WITH the fix the harness is never handed the invocation, so the
/// injected failure is inert and the plan runs to `succeeded`; WITHOUT the fix
/// the invocation is dispatched to the harness, the failure fires, and the
/// operation dead-letters before any phase — so this test fails.
#[tokio::test]
async fn workflow_invocation_is_reducer_driven_not_folded_by_the_harness() {
    let gateway = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(
            FixtureBuilder::new()
                .tenant(TENANT)
                .skill(WF_SKILL, WF_SKILL_BODY)
                .skill("research-angle", ANGLE_SKILL_BODY)
                .skill("research-report", REPORT_SKILL_BODY)
                .skill("workflow-run", RUN_SKILL_BODY)
                .done(),
        ),
        ..Default::default()
    })
    .await;

    let token = gateway.mint_token(TENANT, Role::Admin);
    let port = free_port();
    let listen = format!("127.0.0.1:{port}");
    let ledger_dir = tempfile::tempdir().expect("tempdir for ledger");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_escurel-runner"));
    cmd.env("ESCUREL_RUNNER_LISTEN", &listen)
        .env("ESCUREL_RUNNER_GATEWAY_URL", gateway.base_url())
        .env("ESCUREL_RUNNER_TENANT", TENANT)
        .env("ESCUREL_RUNNER_TOKEN", &token)
        .env("ESCUREL_RUNNER_HARNESS", "echo")
        // Fail any harness run for the PLAN skill — the invocation event's label.
        // With the fix the invocation never reaches the harness, so this is inert;
        // without it, the invocation's harness fold fails and the run dead-letters.
        .env("ESCUREL_ECHO_FAIL_SKILL", WF_SKILL)
        .env(
            "ESCUREL_RUNNER_LEDGER_PATH",
            ledger_dir.path().join("ledger.sqlite").to_str().unwrap(),
        )
        .env("ESCUREL_RUNNER_MAX_DEPTH", "16")
        .env("ESCUREL_RUNNER_MAX_RUNS_PER_ROOT", "64")
        .env("ESCUREL_RUNNER_MAX_ATTEMPTS", "2")
        .env("ESCUREL_RUNNER_RETRY_BACKOFF", "100ms")
        .env("ESCUREL_RUNNER_POLL_INTERVAL", "250ms");
    let _runner = ChildGuard(cmd.spawn().expect("spawn escurel-runner"));

    // Board + invocation created after the runner is READY (startup recovery has
    // run on an empty corpus), so the invocation has a single driver.
    wait_for_runner_ready(&listen).await;
    let run_page = "markdown/instances/workflow-run/rinv.md";
    create_run_board(&gateway, run_page, WF_SKILL).await;
    call_mcp(
        &gateway,
        Role::Admin,
        "capture_event",
        json!({
            "source": "manual",
            "mime": "text/plain",
            "label_skill": WF_SKILL,
            "instance_page_id": run_page,
            "title": "invoke deep-research",
            "body": "Answer the research question.",
            "provenance": {
                "workflow": { "run": run_page, "wf_skill": WF_SKILL, "phase": "invoke" }
            }
        }),
    )
    .await;

    // The reducer emits scope then synthesize; echo runs those (their labels are
    // the produced skills, not the plan skill, so the injected failure never
    // fires). Reaching `succeeded` proves the invocation was reducer-driven —
    // the harness was never handed the run-board fold.
    let succeeded = await_operation_status(&gateway, run_page, "succeeded", 45).await;
    assert!(
        succeeded,
        "the invocation must be reducer-driven: the plan reaches `succeeded` even though \
         the harness is set to fail on the PLAN skill, because the harness is never handed \
         the invocation to fold into the run board"
    );
}

/// Phase 0.3 DoD (no mock): a workflow whose FIRST phase's harness fails must
/// drive the operation to a terminal `failed` status — not wedge at `running`
/// forever. The scope step's echo run is made to fail deterministically
/// (`ESCUREL_ECHO_FAIL_SKILL=research-angle`); it exhausts its retries and
/// dead-letters, and the reducer — driven on that terminal transition, per the
/// step's authored `Stop` outcome — records the operation `failed`.
#[tokio::test]
async fn workflow_first_step_failure_drives_operation_to_terminal_failed() {
    let gateway = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(
            FixtureBuilder::new()
                .tenant(TENANT)
                .skill(WF_SKILL, WF_SKILL_BODY)
                .skill("research-angle", ANGLE_SKILL_BODY)
                .skill("research-report", REPORT_SKILL_BODY)
                .skill("workflow-run", RUN_SKILL_BODY)
                .done(),
        ),
        ..Default::default()
    })
    .await;

    let run_page = "markdown/instances/workflow-run/rfail.md";
    // Board + invocation are created AFTER the runner spawns (below) — startup
    // `recover_workflows` re-drives any non-terminal board that exists at boot,
    // which would race the invocation and re-record `running` after the scope
    // failure's `failed`. In production the runner is long-running and the board
    // never pre-exists at startup; creating it after boot mirrors that.

    // The runner authenticates as an admin identity (in production it mints its
    // own `escurel:admin` bearer): its orchestration writes include the reserved
    // `escurel:run-status` status events, which the capture guard admits only for
    // admin. (The per-run HARNESS token is the separate caller-scoped one, 2c.)
    let token = gateway.mint_token(TENANT, Role::Admin);
    let port = free_port();
    let listen = format!("127.0.0.1:{port}");
    let ledger_dir = tempfile::tempdir().expect("tempdir for ledger");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_escurel-runner"));
    cmd.env("ESCUREL_RUNNER_LISTEN", &listen)
        .env("ESCUREL_RUNNER_GATEWAY_URL", gateway.base_url())
        .env("ESCUREL_RUNNER_TENANT", TENANT)
        .env("ESCUREL_RUNNER_TOKEN", &token)
        .env("ESCUREL_RUNNER_HARNESS", "echo")
        // Make the scope step (produces research-angle) fail every attempt.
        .env("ESCUREL_ECHO_FAIL_SKILL", "research-angle")
        .env(
            "ESCUREL_RUNNER_LEDGER_PATH",
            ledger_dir.path().join("ledger.sqlite").to_str().unwrap(),
        )
        .env("ESCUREL_RUNNER_MAX_DEPTH", "16")
        .env("ESCUREL_RUNNER_MAX_RUNS_PER_ROOT", "64")
        .env("ESCUREL_RUNNER_MAX_ATTEMPTS", "2")
        .env("ESCUREL_RUNNER_RETRY_BACKOFF", "100ms")
        .env("ESCUREL_RUNNER_POLL_INTERVAL", "250ms");
    let _runner = ChildGuard(cmd.spawn().expect("spawn escurel-runner"));

    // Now that the runner is READY (startup recovery has run on an empty corpus),
    // create the board and inject the invocation — the facade's order, and the
    // one that keeps startup recovery from racing the invocation.
    wait_for_runner_ready(&listen).await;
    create_run_board(&gateway, run_page, WF_SKILL).await;
    call_mcp(
        &gateway,
        // A workflow invocation carries `provenance.workflow`, which the gateway
        // now accepts only from an admin/system identity (async-ops 2c-ii — a
        // non-admin caller starts a workflow via `start_operation`, not a raw
        // capture_event). These reducer/barrier tests inject the invocation as
        // that system identity to keep a fixed run-board id for their prefix
        // assertions; the facade path is covered by the start_operation tests.
        Role::Admin,
        "capture_event",
        json!({
            "source": "manual",
            "mime": "text/plain",
            "label_skill": WF_SKILL,
            "instance_page_id": run_page,
            "title": "invoke deep-research (fail scope)",
            "body": "Answer the research question.",
            "provenance": {
                "workflow": { "run": run_page, "wf_skill": WF_SKILL, "phase": "invoke" }
            }
        }),
    )
    .await;

    let failed = await_operation_status(&gateway, run_page, "failed", 30).await;
    assert!(
        failed,
        "a workflow whose first step fails must reach a terminal `failed` status on {run_page}, \
         not wedge at `running`"
    );
    // And it must NOT report success.
    let succeeded = await_operation_status(&gateway, run_page, "succeeded", 1).await;
    assert!(
        !succeeded,
        "a failed operation must not also report succeeded"
    );

    // async-ops Phase 2: `get_operation` derives the terminal `failed`. Admin
    // read: this board was created via capture_event (no `requested_by`).
    let op = call_mcp(
        &gateway,
        Role::Admin,
        "get_operation",
        json!({ "operation_id": run_page }),
    )
    .await;
    assert_eq!(
        op["status"],
        json!("failed"),
        "get_operation derives `failed` for a failed workflow: {op}"
    );
}

// A workflow authored in the PROSE DIALECT (numbered steps in the page body, no
// `phases:` frontmatter), whose single step's authored fallback is `then ask a
// human`. Proves the dialect is actually wired into `WorkflowSkill::parse_page`
// (crew F-1) and that an `AskHuman` fallback drives the operation to
// `awaiting_human` — unreachable before, because YAML phases always defaulted
// to `Stop`.
const PROSE_WF_SKILL: &str = "reorder-flow";
const PROSE_WF_BODY: &str = "---\n\
type: skill\n\
id: reorder-flow\n\
description: Prose-authored workflow test plan.\n\
backend: {kind: workflow}\n\
run_skill: workflow-run\n\
---\n\
# reorder-flow\n\n\
1. Compute the reorder with [[skill::research-angle]].\n   \
on failure: retry once, then ask a human.\n\n\
on any unrecoverable failure: stop and report.\n";

/// Phase 0.3 / crew F-1 + F-3 (no mock): a PROSE-authored workflow whose step
/// fails and authors `then ask a human` drives the operation to
/// `awaiting_human` (not `failed`), proving the dialect front-end is wired in
/// and the authored `AskHuman` policy is honoured end to end.
#[tokio::test]
async fn prose_authored_ask_a_human_fallback_reaches_awaiting_human() {
    let gateway = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(
            FixtureBuilder::new()
                .tenant(TENANT)
                .skill(PROSE_WF_SKILL, PROSE_WF_BODY)
                .skill("research-angle", ANGLE_SKILL_BODY)
                .skill("workflow-run", RUN_SKILL_BODY)
                .done(),
        ),
        ..Default::default()
    })
    .await;

    let run_page = "markdown/instances/workflow-run/rprose.md";
    call_mcp(
        &gateway,
        // A workflow invocation carries `provenance.workflow`, which the gateway
        // now accepts only from an admin/system identity (async-ops 2c-ii — a
        // non-admin caller starts a workflow via `start_operation`, not a raw
        // capture_event). These reducer/barrier tests inject the invocation as
        // that system identity to keep a fixed run-board id for their prefix
        // assertions; the facade path is covered by the start_operation tests.
        Role::Admin,
        "capture_event",
        json!({
            "source": "manual",
            "mime": "text/plain",
            "label_skill": PROSE_WF_SKILL,
            "instance_page_id": run_page,
            "title": "invoke reorder-flow",
            "body": "Compute a reorder.",
            "provenance": {
                "workflow": { "run": run_page, "wf_skill": PROSE_WF_SKILL, "phase": "invoke" }
            }
        }),
    )
    .await;

    // The runner authenticates as an admin identity (in production it mints its
    // own `escurel:admin` bearer): its orchestration writes include the reserved
    // `escurel:run-status` status events, which the capture guard admits only for
    // admin. (The per-run HARNESS token is the separate caller-scoped one, 2c.)
    let token = gateway.mint_token(TENANT, Role::Admin);
    let port = free_port();
    let listen = format!("127.0.0.1:{port}");
    let ledger_dir = tempfile::tempdir().expect("tempdir for ledger");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_escurel-runner"));
    cmd.env("ESCUREL_RUNNER_LISTEN", &listen)
        .env("ESCUREL_RUNNER_GATEWAY_URL", gateway.base_url())
        .env("ESCUREL_RUNNER_TENANT", TENANT)
        .env("ESCUREL_RUNNER_TOKEN", &token)
        .env("ESCUREL_RUNNER_HARNESS", "echo")
        // The step (produces research-angle) fails every attempt → dead-letter
        // → the authored `then ask a human` fallback applies.
        .env("ESCUREL_ECHO_FAIL_SKILL", "research-angle")
        .env(
            "ESCUREL_RUNNER_LEDGER_PATH",
            ledger_dir.path().join("ledger.sqlite").to_str().unwrap(),
        )
        .env("ESCUREL_RUNNER_MAX_DEPTH", "16")
        .env("ESCUREL_RUNNER_MAX_RUNS_PER_ROOT", "64")
        .env("ESCUREL_RUNNER_MAX_ATTEMPTS", "2")
        .env("ESCUREL_RUNNER_RETRY_BACKOFF", "100ms")
        .env("ESCUREL_RUNNER_POLL_INTERVAL", "250ms");
    let _runner = ChildGuard(cmd.spawn().expect("spawn escurel-runner"));

    let awaiting = await_operation_status(&gateway, run_page, "awaiting_human", 30).await;
    assert!(
        awaiting,
        "a prose-authored `ask a human` fallback must drive the operation to `awaiting_human` \
         on {run_page} — proving the dialect is wired in and AskHuman is honoured"
    );
    // It must NOT fail closed to `failed` (that is the Stop path, not AskHuman).
    let failed = await_operation_status(&gateway, run_page, "failed", 1).await;
    assert!(!failed, "an AskHuman fallback must not record `failed`");
}

/// Phase-2 crew security fixes (no mock): F1 — a non-admin agent cannot author a
/// reserved `escurel:run-status` event (which would forge an operation's status);
/// F2 — start_operation refuses a `wf_skill` that is not a readable kind:workflow
/// plan (denial and absence look identical).
#[tokio::test]
async fn facade_refuses_forged_status_and_unknown_plan() {
    let gateway = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(
            FixtureBuilder::new()
                .tenant(TENANT)
                .skill(WF_SKILL, WF_SKILL_BODY)
                .skill("research-angle", ANGLE_SKILL_BODY)
                .skill("workflow-run", RUN_SKILL_BODY)
                .done(),
        ),
        ..Default::default()
    })
    .await;

    // F1: a non-admin caller may not capture a reserved `escurel:` label
    // (deliberately Role::Agent — the negative case being proven).
    let err = call_mcp_err(
        &gateway,
        Role::Agent,
        "capture_event",
        json!({
            "source": "manual",
            "mime": "text/plain",
            "label_skill": "escurel:run-status",
            "instance_page_id": "markdown/instances/workflow-run/victim.md",
            "title": "status: succeeded",
            "provenance": { "run_status": "succeeded" }
        }),
    )
    .await;
    assert!(
        err["message"].as_str().unwrap_or("").contains("reserved"),
        "forged reserved-label capture must be refused: {err}"
    );

    // F2: `wf_skill` that is not a readable kind:workflow plan is refused. A
    // plain skill (`research-angle`, not a workflow) exercises the same gate as
    // an unknown/unreadable id — one error either way.
    let err = call_mcp_err(
        &gateway,
        Role::Agent,
        "start_operation",
        json!({ "wf_skill": "research-angle" }),
    )
    .await;
    assert!(
        err["message"].as_str().unwrap_or("").contains("workflow"),
        "non-workflow plan must be refused: {err}"
    );
    let err = call_mcp_err(
        &gateway,
        Role::Agent,
        "start_operation",
        json!({ "wf_skill": "no-such-plan" }),
    )
    .await;
    assert!(
        err["message"].as_str().unwrap_or("").contains("workflow"),
        "unknown plan must be refused with the same error: {err}"
    );

    // 2c-ii: a non-admin caller cannot forge `provenance.workflow` on a raw
    // capture_event (which would inject a workflow step). The legitimate path is
    // start_operation; the runner emits steps as admin. (A caller's
    // `provenance.runner` block still survives — the narrower guard.)
    let err = call_mcp_err(
        &gateway,
        Role::Agent,
        "capture_event",
        json!({
            "source": "manual",
            "mime": "text/plain",
            "label_skill": "research-angle",
            "instance_page_id": "markdown/instances/workflow-run/forged.md",
            "provenance": { "workflow": { "run": "markdown/instances/workflow-run/forged.md", "wf_skill": WF_SKILL, "phase": "invoke" } }
        }),
    )
    .await;
    assert!(
        err["message"]
            .as_str()
            .unwrap_or("")
            .contains("server-owned"),
        "a forged provenance.workflow must be refused: {err}"
    );
}

/// async-ops Phase 2b (no mock): the `start_operation` facade begins a workflow
/// server-side — the caller names only a plan skill; the server builds the
/// operation id + its workflow provenance and creates the run board — and
/// `get_operation` polls it to `succeeded`. A second call with the same
/// `idempotency_key` re-attaches to the SAME operation (exactly-one-run).
#[tokio::test]
async fn start_operation_begins_a_workflow_and_polls_to_succeeded() {
    let gateway = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(
            FixtureBuilder::new()
                .tenant(TENANT)
                .skill(WF_SKILL, WF_SKILL_BODY)
                .skill("research-angle", ANGLE_SKILL_BODY)
                .skill("research-report", REPORT_SKILL_BODY)
                .skill("workflow-run", RUN_SKILL_BODY)
                .done(),
        ),
        ..Default::default()
    })
    .await;

    // Start via the facade — no caller-supplied provenance, no pre-chosen id.
    let started = call_mcp(
        &gateway,
        Role::Agent,
        "start_operation",
        json!({
            "wf_skill": WF_SKILL,
            "input": "Answer the research question.",
            "idempotency_key": "op-key-1"
        }),
    )
    .await;
    assert_eq!(
        started["status"],
        json!("pending"),
        "starts pending: {started}"
    );
    let operation_id = started["operation_id"]
        .as_str()
        .expect("operation_id")
        .to_owned();
    assert!(
        operation_id.starts_with("markdown/instances/workflow-run/"),
        "operation id is a run board page: {operation_id}"
    );

    // Immediately readable via get_operation (found, not-yet-terminal).
    let now = call_mcp(
        &gateway,
        Role::Agent,
        "get_operation",
        json!({ "operation_id": operation_id }),
    )
    .await;
    assert_eq!(
        now["found"],
        json!(true),
        "operation is found right after start: {now}"
    );

    // Cross-caller denial (crew Phase-2 F3): a DIFFERENT subject must not read
    // this owner-scoped operation — it gets the not-found shape, no leak.
    let intruder = call_mcp_as(
        &gateway,
        Role::Agent,
        "intruder-subject",
        "get_operation",
        json!({ "operation_id": operation_id }),
    )
    .await;
    assert_eq!(
        intruder["found"],
        json!(false),
        "another caller must not read this operation: {intruder}"
    );

    // Idempotency: the same key re-attaches to the same operation.
    let again = call_mcp(
        &gateway,
        Role::Agent,
        "start_operation",
        json!({ "wf_skill": WF_SKILL, "idempotency_key": "op-key-1" }),
    )
    .await;
    assert_eq!(
        again["operation_id"],
        json!(operation_id),
        "same idempotency_key → same operation: {again}"
    );
    assert_eq!(
        again["idempotent"],
        json!(true),
        "re-attach flagged idempotent: {again}"
    );

    // The runner drives the plan (echo, no injected failure) → succeeded.
    // The runner authenticates as an admin identity (in production it mints its
    // own `escurel:admin` bearer): its orchestration writes include the reserved
    // `escurel:run-status` status events, which the capture guard admits only for
    // admin. (The per-run HARNESS token is the separate caller-scoped one, 2c.)
    let token = gateway.mint_token(TENANT, Role::Admin);
    let port = free_port();
    let listen = format!("127.0.0.1:{port}");
    let ledger_dir = tempfile::tempdir().expect("tempdir for ledger");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_escurel-runner"));
    cmd.env("ESCUREL_RUNNER_LISTEN", &listen)
        .env("ESCUREL_RUNNER_GATEWAY_URL", gateway.base_url())
        .env("ESCUREL_RUNNER_TENANT", TENANT)
        .env("ESCUREL_RUNNER_TOKEN", &token)
        .env("ESCUREL_RUNNER_HARNESS", "echo")
        .env(
            "ESCUREL_RUNNER_LEDGER_PATH",
            ledger_dir.path().join("ledger.sqlite").to_str().unwrap(),
        )
        .env("ESCUREL_RUNNER_MAX_DEPTH", "16")
        .env("ESCUREL_RUNNER_MAX_RUNS_PER_ROOT", "64")
        .env("ESCUREL_RUNNER_MAX_ATTEMPTS", "3")
        .env("ESCUREL_RUNNER_RETRY_BACKOFF", "100ms")
        .env("ESCUREL_RUNNER_POLL_INTERVAL", "250ms");
    let _runner = ChildGuard(cmd.spawn().expect("spawn escurel-runner"));

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(45);
    let succeeded = loop {
        let op = call_mcp(
            &gateway,
            Role::Agent,
            "get_operation",
            json!({ "operation_id": operation_id }),
        )
        .await;
        if op["status"] == json!("succeeded") {
            break true;
        }
        if std::time::Instant::now() >= deadline {
            break false;
        }
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    };
    assert!(
        succeeded,
        "the facade-started operation must poll to `succeeded` on {operation_id}"
    );
}

/// async-ops Phase 2c-i (no mock): a background run executes under the
/// REQUESTER's per-run caller token, not the runner's admin identity (the
/// confused-deputy fix). The runner runs in MINTED mode (its own signing key,
/// as in production) so it can mint a short-lived token scoped to the requester
/// the facade stamped on the board; the harness then writes the produced
/// instance under that identity — so the page's `last_written_by` is the
/// requester (`alice-requester`), NOT the runner's own subject (`escurel-runner`).
#[tokio::test]
async fn a_run_executes_under_the_requesters_identity_not_the_runners() {
    let gateway = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(
            FixtureBuilder::new()
                .tenant(TENANT)
                .skill(WF_SKILL, WF_SKILL_BODY)
                .skill("research-angle", ANGLE_SKILL_BODY)
                .skill("research-report", REPORT_SKILL_BODY)
                .skill("workflow-run", RUN_SKILL_BODY)
                .done(),
        ),
        ..Default::default()
    })
    .await;

    // The requester is a distinct subject, so it is distinguishable from the
    // runner's own `escurel-runner` identity on the produced page.
    let requester = "alice-requester";
    let started = call_mcp_as(
        &gateway,
        Role::Agent,
        requester,
        "start_operation",
        json!({ "wf_skill": WF_SKILL, "input": "Answer the question." }),
    )
    .await;
    let operation_id = started["operation_id"]
        .as_str()
        .expect("operation_id")
        .to_owned();
    let run_slug = operation_id
        .strip_prefix("markdown/instances/workflow-run/")
        .and_then(|s| s.strip_suffix(".md"))
        .expect("operation id shape")
        .to_owned();

    // Runner in MINTED mode (production shape) so it can mint the per-run token.
    let (signing_key, kid) = gateway.signing_material();
    let issuer = gateway.issuer_url().to_owned();
    let port = free_port();
    let listen = format!("127.0.0.1:{port}");
    let ledger_dir = tempfile::tempdir().expect("tempdir for ledger");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_escurel-runner"));
    cmd.env("ESCUREL_RUNNER_LISTEN", &listen)
        .env("ESCUREL_RUNNER_GATEWAY_URL", gateway.base_url())
        .env("ESCUREL_RUNNER_TENANT", TENANT)
        .env_remove("ESCUREL_RUNNER_TOKEN")
        .env("ESCUREL_RUNNER_AUTH_ISSUER", &issuer)
        .env("ESCUREL_RUNNER_AUTH_KID", kid)
        .env("ESCUREL_RUNNER_AUTH_SIGNING_KEY", &signing_key)
        .env("ESCUREL_RUNNER_AUTH_SUBJECT", "escurel-runner")
        .env("ESCUREL_RUNNER_HARNESS", "echo")
        .env(
            "ESCUREL_RUNNER_LEDGER_PATH",
            ledger_dir.path().join("ledger.sqlite").to_str().unwrap(),
        )
        .env("ESCUREL_RUNNER_MAX_DEPTH", "16")
        .env("ESCUREL_RUNNER_MAX_RUNS_PER_ROOT", "64")
        .env("ESCUREL_RUNNER_MAX_ATTEMPTS", "3")
        .env("ESCUREL_RUNNER_RETRY_BACKOFF", "100ms")
        .env("ESCUREL_RUNNER_POLL_INTERVAL", "250ms");
    let _runner = ChildGuard(cmd.spawn().expect("spawn escurel-runner"));

    // The scope step produces a run-scoped research-angle instance.
    let angle = await_instance(
        &gateway,
        "research-angle",
        &format!("markdown/instances/research-angle/{run_slug}-scope-"),
        45,
    )
    .await;

    // The decisive assertion: the produced page was written by the REQUESTER's
    // per-run token, not the runner's own admin identity.
    let expanded = call_mcp(&gateway, Role::Admin, "expand", json!({ "page_id": angle })).await;
    let written_by = expanded["page"]["last_written_by"].as_str().unwrap_or("");
    assert_eq!(
        written_by, requester,
        "the run must write as the requester (per-run caller token), not the runner: {expanded}"
    );
    assert_ne!(
        written_by, "escurel-runner",
        "the produced page must not be attributed to the runner's own identity"
    );
}

/// async-ops crew final-review F2 (no mock): a **minting** runner refuses to run
/// a workflow whose board carries NO requester, rather than falling open to its
/// own admin identity. This is the confused-deputy the per-run token closes: the
/// board's `requested_by` is owner-writable, so a non-admin who stripped it
/// mid-operation must not thereby escalate the remaining phases to the runner's
/// authority. Here the board is injected (as the reducer tests do) WITHOUT a
/// requester while the runner runs in minted mode — the packager fails closed,
/// so the scope harness never runs and no `research-angle` instance appears.
#[tokio::test]
async fn a_minting_runner_refuses_a_run_with_no_requester_instead_of_running_as_admin() {
    let gateway = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(
            FixtureBuilder::new()
                .tenant(TENANT)
                .skill(WF_SKILL, WF_SKILL_BODY)
                .skill("research-angle", ANGLE_SKILL_BODY)
                .skill("research-report", REPORT_SKILL_BODY)
                .skill("workflow-run", RUN_SKILL_BODY)
                .done(),
        ),
        ..Default::default()
    })
    .await;

    // Inject the invocation as a system/admin identity onto a fixed board that
    // carries NO `requested_by` (a start_operation board always would).
    let run_page = "markdown/instances/workflow-run/rf2-no-requester.md";
    call_mcp(
        &gateway,
        Role::Admin,
        "capture_event",
        json!({
            "source": "manual",
            "mime": "text/plain",
            "label_skill": WF_SKILL,
            "instance_page_id": run_page,
            "title": "invoke deep-research (no requester)",
            "body": "Answer the research question.",
            "provenance": {
                "workflow": { "run": run_page, "wf_skill": WF_SKILL, "phase": "invoke" }
            }
        }),
    )
    .await;

    // Runner in MINTED mode (production shape): it CAN mint a per-run token, so a
    // board with no requester is the fail-closed case — not the dev-only static
    // fallback.
    let (signing_key, kid) = gateway.signing_material();
    let issuer = gateway.issuer_url().to_owned();
    let port = free_port();
    let listen = format!("127.0.0.1:{port}");
    let ledger_dir = tempfile::tempdir().expect("tempdir for ledger");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_escurel-runner"));
    cmd.env("ESCUREL_RUNNER_LISTEN", &listen)
        .env("ESCUREL_RUNNER_GATEWAY_URL", gateway.base_url())
        .env("ESCUREL_RUNNER_TENANT", TENANT)
        .env_remove("ESCUREL_RUNNER_TOKEN")
        .env("ESCUREL_RUNNER_AUTH_ISSUER", &issuer)
        .env("ESCUREL_RUNNER_AUTH_KID", kid)
        .env("ESCUREL_RUNNER_AUTH_SIGNING_KEY", &signing_key)
        .env("ESCUREL_RUNNER_AUTH_SUBJECT", "escurel-runner")
        .env("ESCUREL_RUNNER_HARNESS", "echo")
        .env(
            "ESCUREL_RUNNER_LEDGER_PATH",
            ledger_dir.path().join("ledger.sqlite").to_str().unwrap(),
        )
        .env("ESCUREL_RUNNER_MAX_DEPTH", "16")
        .env("ESCUREL_RUNNER_MAX_RUNS_PER_ROOT", "64")
        .env("ESCUREL_RUNNER_MAX_ATTEMPTS", "2")
        .env("ESCUREL_RUNNER_RETRY_BACKOFF", "100ms")
        .env("ESCUREL_RUNNER_POLL_INTERVAL", "250ms");
    let _runner = ChildGuard(cmd.spawn().expect("spawn escurel-runner"));

    // The decisive assertion: over a window in which a permitted run would have
    // produced the scope output, NO `research-angle` instance appears — the run
    // fails closed at packaging (dead-letter, `Permanent`), never executing under
    // the runner's admin identity.
    let deadline = Instant::now() + Duration::from_secs(6);
    while Instant::now() < deadline {
        let r = call_mcp(
            &gateway,
            Role::Admin,
            "list_instances",
            json!({ "skill_id": "research-angle" }),
        )
        .await;
        let produced = r["instances"].as_array().is_some_and(|is| !is.is_empty());
        assert!(
            !produced,
            "a minting runner must NOT execute a requester-less run: it produced {r}"
        );
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
}

/// async-ops Phase 3 (no mock): a terminal operation is delivered back to the
/// channel that started it. An operation is started with a `conversation_ref`
/// (a simulated Teams turn); when the runner drives it to `succeeded`, it POSTs
/// the terminal result to the channel courier's `/v1/outbound` seam — a stub
/// sink here — keyed on that stored conversation reference.
#[tokio::test]
async fn a_terminal_operation_is_delivered_to_the_channel_courier() {
    let gateway = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(
            FixtureBuilder::new()
                .tenant(TENANT)
                .skill(WF_SKILL, WF_SKILL_BODY)
                .skill("research-angle", ANGLE_SKILL_BODY)
                .skill("research-report", REPORT_SKILL_BODY)
                .skill("workflow-run", RUN_SKILL_BODY)
                .done(),
        ),
        ..Default::default()
    })
    .await;

    let (sink_url, received) = spawn_outbound_sink().await;

    // Start the operation with a channel reference (a simulated Teams turn).
    let conversation_ref = json!({
        "channel": "msteams",
        "conversation": { "id": "19:meeting_abc@thread.v2" },
        "service_url": "https://smba.example/teams"
    });
    let started = call_mcp(
        &gateway,
        Role::Agent,
        "start_operation",
        json!({
            "wf_skill": WF_SKILL,
            "input": "Answer the question.",
            "conversation_ref": conversation_ref,
            // The CHANNEL's tenant — the chat platform's, not escurel's.
            "channel_tenant": "acme-tenant-guid",
        }),
    )
    .await;
    let operation_id = started["operation_id"]
        .as_str()
        .expect("operation_id")
        .to_owned();

    // Runner wired to the channel courier's outbound seam.
    let token = gateway.mint_token(TENANT, Role::Admin);
    let port = free_port();
    let listen = format!("127.0.0.1:{port}");
    let ledger_dir = tempfile::tempdir().expect("tempdir for ledger");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_escurel-runner"));
    cmd.env("ESCUREL_RUNNER_LISTEN", &listen)
        .env("ESCUREL_RUNNER_GATEWAY_URL", gateway.base_url())
        .env("ESCUREL_RUNNER_TENANT", TENANT)
        .env("ESCUREL_RUNNER_TOKEN", &token)
        .env("ESCUREL_RUNNER_HARNESS", "echo")
        .env("ESCUREL_RUNNER_OUTBOUND_URL", &sink_url)
        .env(
            "ESCUREL_RUNNER_LEDGER_PATH",
            ledger_dir.path().join("ledger.sqlite").to_str().unwrap(),
        )
        .env("ESCUREL_RUNNER_MAX_DEPTH", "16")
        .env("ESCUREL_RUNNER_MAX_RUNS_PER_ROOT", "64")
        .env("ESCUREL_RUNNER_MAX_ATTEMPTS", "3")
        .env("ESCUREL_RUNNER_RETRY_BACKOFF", "100ms")
        .env("ESCUREL_RUNNER_POLL_INTERVAL", "250ms");
    let _runner = ChildGuard(cmd.spawn().expect("spawn escurel-runner"));

    // The courier receives the TERMINAL delivery, keyed on the conversation ref.
    // (Selective progress deliveries — status `running` — also arrive for this
    // operation now; find the terminal one specifically.)
    let deadline = Instant::now() + Duration::from_secs(45);
    let delivered = loop {
        let hit = received
            .lock()
            .expect("sink mutex")
            .iter()
            .find(|d| {
                d["operation_id"].as_str() == Some(operation_id.as_str())
                    && d["status"].as_str() == Some("succeeded")
            })
            .cloned();
        if let Some(d) = hit {
            break Some(d);
        }
        if Instant::now() >= deadline {
            break None;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    };
    let delivery = delivered.expect("the terminal operation must be delivered to the courier");
    assert_eq!(
        delivery["status"],
        json!("succeeded"),
        "delivery carries the terminal status: {delivery}"
    );
    assert_eq!(
        delivery["conversation_ref"], conversation_ref,
        "delivery carries the stored conversation reference verbatim: {delivery}"
    );
    // The CHANNEL's tenant, recorded when the operation started and echoed
    // back independently of the reference.
    //
    // Without it the delivery side can only compare fields the caller wrote
    // with each other: triton's courier checked the reference's tenant
    // against the caller's own, and the caller supplied both
    // (DataZooDE/triton#332). A tenant fixed at START is the second,
    // independent side that check needs.
    assert_eq!(
        delivery["channel_tenant"], "acme-tenant-guid",
        "delivery must carry the channel tenant recorded at start: {delivery}"
    );
}

/// async-ops Phase 3b (no mock): a multi-phase operation pushes SELECTIVE
/// progress back to the channel — one delivery per PHASE BOUNDARY (status
/// `running`, carrying a "⏳ Working on *<phase>*…" note the courier renders),
/// never one per step. The 2-phase plan yields a progress push for `scope` and
/// one for `synthesize`, alongside the terminal — so a user watching the chat
/// sees the operation advance instead of only its final result.
#[tokio::test]
async fn a_multi_phase_operation_pushes_selective_progress() {
    let gateway = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(
            FixtureBuilder::new()
                .tenant(TENANT)
                .skill(WF_SKILL, WF_SKILL_BODY)
                .skill("research-angle", ANGLE_SKILL_BODY)
                .skill("research-report", REPORT_SKILL_BODY)
                .skill("workflow-run", RUN_SKILL_BODY)
                .done(),
        ),
        ..Default::default()
    })
    .await;

    let (sink_url, received) = spawn_outbound_sink().await;

    // Spawn the runner FIRST and let startup recovery run on an empty corpus, so
    // the operation has a SINGLE driver (the poller) — otherwise startup recovery
    // races the invocation and the per-phase progress boundary is hit twice /
    // silently, exactly like the reducer tests.
    let token = gateway.mint_token(TENANT, Role::Admin);
    let port = free_port();
    let listen = format!("127.0.0.1:{port}");
    let ledger_dir = tempfile::tempdir().expect("tempdir for ledger");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_escurel-runner"));
    cmd.env("ESCUREL_RUNNER_LISTEN", &listen)
        .env("ESCUREL_RUNNER_GATEWAY_URL", gateway.base_url())
        .env("ESCUREL_RUNNER_TENANT", TENANT)
        .env("ESCUREL_RUNNER_TOKEN", &token)
        .env("ESCUREL_RUNNER_HARNESS", "echo")
        .env("ESCUREL_RUNNER_OUTBOUND_URL", &sink_url)
        .env(
            "ESCUREL_RUNNER_LEDGER_PATH",
            ledger_dir.path().join("ledger.sqlite").to_str().unwrap(),
        )
        .env("ESCUREL_RUNNER_MAX_DEPTH", "16")
        .env("ESCUREL_RUNNER_MAX_RUNS_PER_ROOT", "64")
        .env("ESCUREL_RUNNER_MAX_ATTEMPTS", "3")
        .env("ESCUREL_RUNNER_RETRY_BACKOFF", "100ms")
        .env("ESCUREL_RUNNER_POLL_INTERVAL", "250ms");
    let _runner = ChildGuard(cmd.spawn().expect("spawn escurel-runner"));
    wait_for_runner_ready(&listen).await;

    let conversation_ref = json!({
        "channel": "msteams",
        "conversation": { "id": "19:progress@thread.v2" },
        "service_url": "https://smba.example/teams"
    });
    let started = call_mcp(
        &gateway,
        Role::Agent,
        "start_operation",
        json!({
            "wf_skill": WF_SKILL,
            "input": "Answer the question.",
            "conversation_ref": conversation_ref,
        }),
    )
    .await;
    let operation_id = started["operation_id"]
        .as_str()
        .expect("operation_id")
        .to_owned();

    // Wait until the terminal arrives, then inspect the whole delivery stream.
    let deadline = Instant::now() + Duration::from_secs(45);
    loop {
        let has_terminal = received.lock().expect("sink mutex").iter().any(|d| {
            d["operation_id"].as_str() == Some(operation_id.as_str())
                && d["status"].as_str() == Some("succeeded")
        });
        if has_terminal {
            break;
        }
        if Instant::now() >= deadline {
            panic!("the operation never reached a terminal delivery");
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }

    let deliveries = received.lock().expect("sink mutex").clone();
    let notes: Vec<String> = deliveries
        .iter()
        .filter(|d| {
            d["operation_id"].as_str() == Some(operation_id.as_str())
                && d["status"].as_str() == Some("running")
        })
        .filter_map(|d| d["result"]["text"].as_str().map(str::to_owned))
        .collect();
    assert!(
        notes.iter().any(|n| n.contains("scope")),
        "a progress push announced the scope phase: {notes:?}"
    );
    assert!(
        notes.iter().any(|n| n.contains("synthesize")),
        "a progress push announced the synthesize phase: {notes:?}"
    );
    // Selective: one push per phase boundary, not a flood per step.
    assert!(
        notes.len() <= 3,
        "progress is per-phase, not per-step ({} pushes): {notes:?}",
        notes.len()
    );

    gateway.shutdown().await;
}

/// async-ops Phase 3 (no mock): the terminal delivery carries the configured
/// server-to-server bearer. The agent's delivery receiver refuses a callback with
/// no/wrong `Authorization` header (401), so a runner that does not present
/// `ESCUREL_RUNNER_OUTBOUND_BEARER` would deliver NOTHING live. The sink here
/// mirrors that: it records only bearer-authenticated POSTs. This is the
/// regression guard for the auth gap between the runner (sender) and the agent
/// receiver.
#[tokio::test]
async fn a_terminal_delivery_carries_the_configured_bearer() {
    const BEARER: &str = "s3cret-callback-bearer";
    let gateway = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(
            FixtureBuilder::new()
                .tenant(TENANT)
                .skill(WF_SKILL, WF_SKILL_BODY)
                .skill("research-angle", ANGLE_SKILL_BODY)
                .skill("research-report", REPORT_SKILL_BODY)
                .skill("workflow-run", RUN_SKILL_BODY)
                .done(),
        ),
        ..Default::default()
    })
    .await;

    let (sink_url, received) = spawn_outbound_sink_requiring_bearer(BEARER).await;

    let conversation_ref = json!({
        "channel": "msteams",
        "conversation": { "id": "19:meeting_xyz@thread.v2" },
        "service_url": "https://smba.example/teams"
    });
    let started = call_mcp(
        &gateway,
        Role::Agent,
        "start_operation",
        json!({
            "wf_skill": WF_SKILL,
            "input": "Answer the question.",
            "conversation_ref": conversation_ref,
        }),
    )
    .await;
    let operation_id = started["operation_id"]
        .as_str()
        .expect("operation_id")
        .to_owned();

    let token = gateway.mint_token(TENANT, Role::Admin);
    let port = free_port();
    let listen = format!("127.0.0.1:{port}");
    let ledger_dir = tempfile::tempdir().expect("tempdir for ledger");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_escurel-runner"));
    cmd.env("ESCUREL_RUNNER_LISTEN", &listen)
        .env("ESCUREL_RUNNER_GATEWAY_URL", gateway.base_url())
        .env("ESCUREL_RUNNER_TENANT", TENANT)
        .env("ESCUREL_RUNNER_TOKEN", &token)
        .env("ESCUREL_RUNNER_HARNESS", "echo")
        .env("ESCUREL_RUNNER_OUTBOUND_URL", &sink_url)
        .env("ESCUREL_RUNNER_OUTBOUND_BEARER", BEARER)
        .env(
            "ESCUREL_RUNNER_LEDGER_PATH",
            ledger_dir.path().join("ledger.sqlite").to_str().unwrap(),
        )
        .env("ESCUREL_RUNNER_MAX_DEPTH", "16")
        .env("ESCUREL_RUNNER_MAX_RUNS_PER_ROOT", "64")
        .env("ESCUREL_RUNNER_MAX_ATTEMPTS", "3")
        .env("ESCUREL_RUNNER_RETRY_BACKOFF", "100ms")
        .env("ESCUREL_RUNNER_POLL_INTERVAL", "250ms");
    let _runner = ChildGuard(cmd.spawn().expect("spawn escurel-runner"));

    // The bearer-gated sink only records the POST if the runner authenticated.
    // Find the TERMINAL delivery (progress `running` deliveries also arrive).
    let deadline = Instant::now() + Duration::from_secs(45);
    let delivered = loop {
        let hit = received
            .lock()
            .expect("sink mutex")
            .iter()
            .find(|d| {
                d["operation_id"].as_str() == Some(operation_id.as_str())
                    && d["status"].as_str() == Some("succeeded")
            })
            .cloned();
        if let Some(d) = hit {
            break Some(d);
        }
        if Instant::now() >= deadline {
            break None;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    };
    let delivery = delivered.expect(
        "the terminal delivery must authenticate with the configured bearer and be recorded",
    );
    assert_eq!(
        delivery["status"],
        json!("succeeded"),
        "the bearer-authenticated delivery carries the terminal status: {delivery}"
    );
    assert_eq!(
        delivery["conversation_ref"], conversation_ref,
        "the bearer-authenticated delivery carries the conversation reference: {delivery}"
    );
}

#[tokio::test]
async fn verify_barrier_runs_to_completion_via_echo() {
    // The width-3 adversarial **verify barrier**, driven DETERMINISTICALLY by
    // the echo harness — the headless companion to
    // `verify_barrier_runs_against_gemini`. The barrier tally counts
    // `COUNT(DISTINCT vote_index)` per claim, so the harness that produces a
    // `verify-vote` MUST stamp `claim` + `vote_index` into its frontmatter
    // (`§3.5`, and the `vote_index` doc-contract on `WorkflowProvenance`).
    // The echo harness recovers both from the step's `provenance.workflow`
    // (`over` → claim, `vote_index` → slot). Without that, all three votes
    // collapse to one slot and the barrier wedges open forever — this test is
    // the regression guard for that.
    let gateway = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(
            FixtureBuilder::new()
                .tenant(TENANT)
                .skill(VERIFY_WF_SKILL, VERIFY_WF_BODY)
                .skill("claims", CLAIMS_SKILL_BODY)
                .skill("verify-vote", VERIFY_VOTE_SKILL_BODY)
                .skill("research-report", REPORT_SKILL_BODY)
                .skill("workflow-run", RUN_SKILL_BODY)
                .done(),
        ),
        ..Default::default()
    })
    .await;

    let run_page = "markdown/instances/workflow-run/echobar.md";
    call_mcp(
        &gateway,
        // A workflow invocation carries `provenance.workflow`, which the gateway
        // now accepts only from an admin/system identity (async-ops 2c-ii — a
        // non-admin caller starts a workflow via `start_operation`, not a raw
        // capture_event). These reducer/barrier tests inject the invocation as
        // that system identity to keep a fixed run-board id for their prefix
        // assertions; the facade path is covered by the start_operation tests.
        Role::Admin,
        "capture_event",
        json!({
            "source": "manual",
            "mime": "text/plain",
            "label_skill": VERIFY_WF_SKILL,
            "instance_page_id": run_page,
            "title": "invoke claim-check (echo)",
            "body": "Extract claims, verify them, synthesize.",
            "provenance": { "workflow": { "run": run_page, "wf_skill": VERIFY_WF_SKILL, "phase": "invoke" } }
        }),
    )
    .await;

    // The runner authenticates as an admin identity (in production it mints its
    // own `escurel:admin` bearer): its orchestration writes include the reserved
    // `escurel:run-status` status events, which the capture guard admits only for
    // admin. (The per-run HARNESS token is the separate caller-scoped one, 2c.)
    let token = gateway.mint_token(TENANT, Role::Admin);
    let port = free_port();
    let listen = format!("127.0.0.1:{port}");
    let ledger_dir = tempfile::tempdir().expect("tempdir for ledger");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_escurel-runner"));
    cmd.env("ESCUREL_RUNNER_LISTEN", &listen)
        .env("ESCUREL_RUNNER_GATEWAY_URL", gateway.base_url())
        .env("ESCUREL_RUNNER_TENANT", TENANT)
        .env("ESCUREL_RUNNER_TOKEN", &token)
        .env("ESCUREL_RUNNER_HARNESS", "echo")
        .env(
            "ESCUREL_RUNNER_LEDGER_PATH",
            ledger_dir.path().join("ledger.sqlite").to_str().unwrap(),
        )
        .env("ESCUREL_RUNNER_MAX_DEPTH", "16")
        .env("ESCUREL_RUNNER_MAX_RUNS_PER_ROOT", "64")
        .env("ESCUREL_RUNNER_MAX_ATTEMPTS", "3")
        .env("ESCUREL_RUNNER_RETRY_BACKOFF", "100ms")
        .env("ESCUREL_RUNNER_POLL_INTERVAL", "250ms");
    let _runner = ChildGuard(cmd.spawn().expect("spawn escurel-runner"));

    // The barrier fans out three skeptic votes at distinct slots…
    let votes = await_instances(
        &gateway,
        "verify-vote",
        "markdown/instances/verify-vote/echobar-verify-",
        3,
        45,
    )
    .await;
    let mut indices = Vec::new();
    for page in &votes {
        let e = call_mcp(&gateway, Role::Agent, "expand", json!({ "page_id": page })).await;
        indices.push(
            e["frontmatter"]["vote_index"]
                .as_u64()
                .unwrap_or_else(|| panic!("echo vote {page} missing vote_index: {e}")),
        );
    }
    indices.sort_unstable();
    indices.dedup();
    assert_eq!(
        indices.len(),
        3,
        "three DISTINCT vote_index slots: {indices:?}"
    );

    // …and only once the barrier CLOSES does synthesize fire. The report
    // proves the echo-authored votes tallied to quorum.
    let report = await_instance(
        &gateway,
        "research-report",
        "markdown/instances/research-report/echobar-synthesize-",
        45,
    )
    .await;
    assert!(report.ends_with(".md"), "report page id: {report}");
}

/// LIVE test: drive the workflow through a real **Gemini** harness (env-guarded
/// on GEMINI_API_KEY, like the other `*_live` adapters). It runs the SHIPPED
/// `gemini` adapter — the one the cluster dispatches with — so each phase's
/// instance body is authored by Gemini over the real `/mcp` surface, through
/// the same in-process tool loop production uses. It used to drive a Python
/// runner through the ADK adapter, which tested a harness nobody deploys.
/// Run with:  GEMINI_API_KEY=… cargo test -p escurel-runner --test
/// workflow_end_to_end deep_research_runs_against_gemini -- --nocapture
#[tokio::test]
async fn deep_research_runs_against_gemini() {
    if std::env::var("GEMINI_API_KEY").is_err() {
        eprintln!("skipping: GEMINI_API_KEY not set");
        return;
    }
    let gateway = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(
            FixtureBuilder::new()
                .tenant(TENANT)
                .skill(WF_SKILL, WF_SKILL_BODY)
                .skill("research-angle", ANGLE_SKILL_BODY)
                .skill("research-report", REPORT_SKILL_BODY)
                .skill("workflow-run", RUN_SKILL_BODY)
                .done(),
        ),
        ..Default::default()
    })
    .await;

    let run_page = "markdown/instances/workflow-run/gem.md";
    let question = "Why is the sky blue during the day but red at sunset? \
                    Give the physics (Rayleigh scattering) and the key factors.";
    call_mcp(
        &gateway,
        // A workflow invocation carries `provenance.workflow`, which the gateway
        // now accepts only from an admin/system identity (async-ops 2c-ii — a
        // non-admin caller starts a workflow via `start_operation`, not a raw
        // capture_event). These reducer/barrier tests inject the invocation as
        // that system identity to keep a fixed run-board id for their prefix
        // assertions; the facade path is covered by the start_operation tests.
        Role::Admin,
        "capture_event",
        json!({
            "source": "manual",
            "mime": "text/plain",
            "label_skill": WF_SKILL,
            "instance_page_id": run_page,
            "title": "invoke deep-research (gemini)",
            "body": question,
            "provenance": { "workflow": { "run": run_page, "wf_skill": WF_SKILL, "phase": "invoke" } }
        }),
    )
    .await;

    // The runner authenticates as an admin identity (in production it mints its
    // own `escurel:admin` bearer): its orchestration writes include the reserved
    // `escurel:run-status` status events, which the capture guard admits only for
    // admin. (The per-run HARNESS token is the separate caller-scoped one, 2c.)
    let token = gateway.mint_token(TENANT, Role::Admin);
    let port = free_port();
    let listen = format!("127.0.0.1:{port}");
    let ledger_dir = tempfile::tempdir().expect("tempdir for ledger");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_escurel-runner"));
    cmd.env("ESCUREL_RUNNER_LISTEN", &listen)
        .env("ESCUREL_RUNNER_GATEWAY_URL", gateway.base_url())
        .env("ESCUREL_RUNNER_TENANT", TENANT)
        .env("ESCUREL_RUNNER_TOKEN", &token)
        .env("ESCUREL_RUNNER_HARNESS", "gemini")
        .env(
            "ESCUREL_GEMINI_API_KEY",
            std::env::var("GEMINI_API_KEY").expect("guarded above"),
        )
        .env(
            "ESCUREL_RUNNER_LEDGER_PATH",
            ledger_dir.path().join("ledger.sqlite").to_str().unwrap(),
        )
        .env("ESCUREL_RUNNER_MAX_DEPTH", "16")
        .env("ESCUREL_RUNNER_MAX_RUNS_PER_ROOT", "64")
        .env("ESCUREL_RUNNER_MAX_ATTEMPTS", "2")
        .env("ESCUREL_RUNNER_RETRY_BACKOFF", "500ms")
        .env("ESCUREL_RUNNER_POLL_INTERVAL", "500ms");
    let _runner = ChildGuard(cmd.spawn().expect("spawn escurel-runner"));

    // Real Gemini calls per phase → allow a generous deadline.
    let angle = await_instance(
        &gateway,
        "research-angle",
        "markdown/instances/research-angle/gem-scope-",
        120,
    )
    .await;
    let report = await_instance(
        &gateway,
        "research-report",
        "markdown/instances/research-report/gem-synthesize-",
        120,
    )
    .await;

    // Show the REAL Gemini-authored bodies.
    let show = |label: &str, page: &str| {
        let g = &gateway;
        let page = page.to_owned();
        let label = label.to_owned();
        async move {
            let e = call_mcp(g, Role::Agent, "expand", json!({ "page_id": page })).await;
            eprintln!(
                "\n===== {label} ({page}) =====\n{}\n",
                e["body"].as_str().unwrap_or("")
            );
        }
    };
    show("SCOPE → research-angle", &angle).await;
    show("SYNTHESIZE → research-report", &report).await;

    assert!(report.ends_with(".md"), "gemini produced a research-report");
    gateway.shutdown().await;
}

/// Poll `list_instances(skill)` until at least `n` instances whose page ids
/// start with `prefix` exist, or the deadline passes (returns their page ids).
async fn await_instances(
    p: &EscurelProcess,
    skill: &str,
    prefix: &str,
    n: usize,
    secs: u64,
) -> Vec<String> {
    let deadline = Instant::now() + Duration::from_secs(secs);
    loop {
        let r = call_mcp(
            p,
            Role::Agent,
            "list_instances",
            json!({ "skill_id": skill }),
        )
        .await;
        let pages: Vec<String> = r["instances"]
            .as_array()
            .map(|is| {
                is.iter()
                    .filter_map(|i| i["page_id"].as_str())
                    .filter(|pid| pid.starts_with(prefix))
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default();
        if pages.len() >= n {
            return pages;
        }
        if Instant::now() >= deadline {
            panic!(
                "only {} of {n} {skill} instances (prefix {prefix}) within {secs}s",
                pages.len()
            );
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

/// LIVE barrier test: drive the width-3 adversarial **verify barrier** through
/// real **Gemini** (env-guarded on GEMINI_API_KEY). This is the follow-up to
/// `deep_research_runs_against_gemini`: where that run was linear
/// (scope → synthesize), this one forces the quorum barrier — three skeptics
/// each author a real `verify-vote` at a distinct `vote_index` (carried in
/// `provenance.workflow.vote_index`), the reducer tallies `COUNT(DISTINCT
/// vote_index)`, and only when the barrier closes does `synthesize` fire.
/// Run with:  GEMINI_API_KEY=… cargo test -p escurel-runner --test
/// workflow_end_to_end verify_barrier_runs_against_gemini -- --nocapture
#[tokio::test]
async fn verify_barrier_runs_against_gemini() {
    if std::env::var("GEMINI_API_KEY").is_err() {
        eprintln!("skipping: GEMINI_API_KEY not set");
        return;
    }
    let gateway = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(
            FixtureBuilder::new()
                .tenant(TENANT)
                .skill(VERIFY_WF_SKILL, VERIFY_WF_BODY)
                .skill("claims", CLAIMS_SKILL_BODY)
                .skill("verify-vote", VERIFY_VOTE_SKILL_BODY)
                .skill("research-report", REPORT_SKILL_BODY)
                .skill("workflow-run", RUN_SKILL_BODY)
                .done(),
        ),
        ..Default::default()
    })
    .await;

    let run_page = "markdown/instances/workflow-run/vfy.md";
    let question = "Is the Great Wall of China visible to the naked eye from the Moon? \
                    State the factual claims and the physics of human visual acuity.";
    call_mcp(
        &gateway,
        // A workflow invocation carries `provenance.workflow`, which the gateway
        // now accepts only from an admin/system identity (async-ops 2c-ii — a
        // non-admin caller starts a workflow via `start_operation`, not a raw
        // capture_event). These reducer/barrier tests inject the invocation as
        // that system identity to keep a fixed run-board id for their prefix
        // assertions; the facade path is covered by the start_operation tests.
        Role::Admin,
        "capture_event",
        json!({
            "source": "manual",
            "mime": "text/plain",
            "label_skill": VERIFY_WF_SKILL,
            "instance_page_id": run_page,
            "title": "invoke claim-check (gemini)",
            "body": question,
            "provenance": { "workflow": { "run": run_page, "wf_skill": VERIFY_WF_SKILL, "phase": "invoke" } }
        }),
    )
    .await;

    // The runner authenticates as an admin identity (in production it mints its
    // own `escurel:admin` bearer): its orchestration writes include the reserved
    // `escurel:run-status` status events, which the capture guard admits only for
    // admin. (The per-run HARNESS token is the separate caller-scoped one, 2c.)
    let token = gateway.mint_token(TENANT, Role::Admin);
    let port = free_port();
    let listen = format!("127.0.0.1:{port}");
    let ledger_dir = tempfile::tempdir().expect("tempdir for ledger");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_escurel-runner"));
    cmd.env("ESCUREL_RUNNER_LISTEN", &listen)
        .env("ESCUREL_RUNNER_GATEWAY_URL", gateway.base_url())
        .env("ESCUREL_RUNNER_TENANT", TENANT)
        .env("ESCUREL_RUNNER_TOKEN", &token)
        .env("ESCUREL_RUNNER_HARNESS", "gemini")
        .env(
            "ESCUREL_GEMINI_API_KEY",
            std::env::var("GEMINI_API_KEY").expect("guarded above"),
        )
        .env(
            "ESCUREL_RUNNER_LEDGER_PATH",
            ledger_dir.path().join("ledger.sqlite").to_str().unwrap(),
        )
        .env("ESCUREL_RUNNER_MAX_DEPTH", "16")
        .env("ESCUREL_RUNNER_MAX_RUNS_PER_ROOT", "64")
        .env("ESCUREL_RUNNER_MAX_ATTEMPTS", "2")
        .env("ESCUREL_RUNNER_RETRY_BACKOFF", "500ms")
        .env("ESCUREL_RUNNER_POLL_INTERVAL", "500ms");
    let _runner = ChildGuard(cmd.spawn().expect("spawn escurel-runner"));

    // The barrier: three skeptic verify-vote instances, each at its own slot.
    let votes = await_instances(
        &gateway,
        "verify-vote",
        "markdown/instances/verify-vote/vfy-verify-",
        3,
        180,
    )
    .await;
    // Only synthesize once the barrier CLOSES — the report proves the tally
    // released the Fixed synthesize phase.
    let report = await_instance(
        &gateway,
        "research-report",
        "markdown/instances/research-report/vfy-synthesize-",
        180,
    )
    .await;

    // The three votes must carry three DISTINCT vote_index values — the whole
    // point of threading the slot through provenance. Read them back and check.
    let mut indices = Vec::new();
    for page in &votes {
        let e = call_mcp(&gateway, Role::Agent, "expand", json!({ "page_id": page })).await;
        let fm = &e["frontmatter"];
        let vi = fm["vote_index"].as_u64().expect("vote has a vote_index");
        let verdict = fm["verdict"].as_str().unwrap_or("").to_owned();
        indices.push(vi);
        eprintln!(
            "\n===== VERIFY-VOTE #{vi} ({page}) verdict={verdict} =====\n{}\n",
            e["body"].as_str().unwrap_or("")
        );
    }
    indices.sort_unstable();
    indices.dedup();
    assert_eq!(
        indices.len(),
        3,
        "three distinct vote_index slots: {indices:?}"
    );

    let e = call_mcp(
        &gateway,
        Role::Agent,
        "expand",
        json!({ "page_id": report }),
    )
    .await;
    eprintln!(
        "\n===== SYNTHESIZE → research-report ({report}) =====\n{}\n",
        e["body"].as_str().unwrap_or("")
    );
    assert!(report.ends_with(".md"), "gemini produced a research-report");
    gateway.shutdown().await;
}

#[tokio::test]
async fn deep_research_corpus_loads_into_a_real_tenant() {
    // The flagship corpus (§8 step 11) seeds into a real gateway: the plan is
    // a `kind: workflow` skill, its five typed produced skills + the run board
    // are present, and the verify-tally inspection query is queryable.
    let mut tf = FixtureBuilder::new().tenant(TENANT);
    for (page_id, body) in escurel_runner_workflow::corpus::deep_research_corpus() {
        tf = tf.page(&page_id, body);
    }
    let gateway = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(tf.done()),
        ..Default::default()
    })
    .await;

    let skills = call_mcp(&gateway, Role::Agent, "list_skills", json!({})).await;
    let arr = skills["skills"].as_array().expect("skills array");
    let by_id = |id: &str| arr.iter().find(|s| s["id"] == id).cloned();

    let plan = by_id("deep-research").expect("deep-research plan present");
    assert_eq!(plan["backend"]["kind"], "workflow");
    for typed in [
        "research-angle",
        "source",
        "claims",
        "verify-vote",
        "research-report",
        "workflow-run",
    ] {
        assert!(by_id(typed).is_some(), "typed skill {typed} present");
    }

    // The verify-tally inspection query is a `query` instance.
    let queries = call_mcp(
        &gateway,
        Role::Agent,
        "list_instances",
        json!({ "skill_id": "query" }),
    )
    .await;
    assert!(
        queries["instances"]
            .as_array()
            .unwrap()
            .iter()
            .any(|i| i["frontmatter"]["id"] == "verify-tally"),
        "verify-tally query shipped: {queries}"
    );

    gateway.shutdown().await;
}

#[tokio::test]
async fn recovery_re_drives_a_non_terminal_run_to_completion() {
    // Simulate a crash mid-run: the run board exists (carrying `wf_skill`) and
    // scope has already produced its research-angle instance, but synthesize
    // never fired. No invocation event is in the inbox. On startup the
    // workflow-aware recovery pass must re-invoke the reducer, see scope
    // complete, emit synthesize, and drive the run to a research-report —
    // proving resume survives process death (§7).
    let run_page = "markdown/instances/workflow-run/rec.md";
    // The board records which plan it belongs to (recovery reads `wf_skill`).
    let board_body = "---\ntype: instance\nskill: workflow-run\nid: rec\n\
         wf_skill: deep-research\n---\n# run rec\n";
    // Scope's produced instance, at its DETERMINISTIC pre-flagged page id.
    let angle_page = escurel_runner_workflow::key::step_instance_page_id(
        "research-angle",
        run_page,
        "scope",
        "0",
    );
    let angle_id = angle_page
        .strip_prefix("markdown/instances/research-angle/")
        .unwrap()
        .strip_suffix(".md")
        .unwrap();
    let angle_body =
        format!("---\ntype: instance\nskill: research-angle\nid: {angle_id}\n---\n# angle\n");

    let gateway = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(
            FixtureBuilder::new()
                .tenant(TENANT)
                .skill(WF_SKILL, WF_SKILL_BODY)
                .skill("research-angle", ANGLE_SKILL_BODY)
                .skill("research-report", REPORT_SKILL_BODY)
                .skill("workflow-run", RUN_SKILL_BODY)
                .instance("workflow-run", "rec", board_body)
                .instance("research-angle", angle_id, angle_body.as_str())
                .done(),
        ),
        ..Default::default()
    })
    .await;

    // Sanity: no research-report yet.
    let before = call_mcp(
        &gateway,
        Role::Agent,
        "list_instances",
        json!({ "skill_id": "research-report" }),
    )
    .await;
    assert_eq!(before["instances"].as_array().map_or(0, Vec::len), 0);

    // Start the runner fresh — recovery runs at startup.
    // The runner authenticates as an admin identity (in production it mints its
    // own `escurel:admin` bearer): its orchestration writes include the reserved
    // `escurel:run-status` status events, which the capture guard admits only for
    // admin. (The per-run HARNESS token is the separate caller-scoped one, 2c.)
    let token = gateway.mint_token(TENANT, Role::Admin);
    let port = free_port();
    let listen = format!("127.0.0.1:{port}");
    let ledger_dir = tempfile::tempdir().expect("tempdir for ledger");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_escurel-runner"));
    cmd.env("ESCUREL_RUNNER_LISTEN", &listen)
        .env("ESCUREL_RUNNER_GATEWAY_URL", gateway.base_url())
        .env("ESCUREL_RUNNER_TENANT", TENANT)
        .env("ESCUREL_RUNNER_TOKEN", &token)
        .env("ESCUREL_RUNNER_HARNESS", "echo")
        .env(
            "ESCUREL_RUNNER_LEDGER_PATH",
            ledger_dir.path().join("ledger.sqlite").to_str().unwrap(),
        )
        .env("ESCUREL_RUNNER_MAX_DEPTH", "16")
        .env("ESCUREL_RUNNER_MAX_RUNS_PER_ROOT", "64")
        .env("ESCUREL_RUNNER_MAX_ATTEMPTS", "3")
        .env("ESCUREL_RUNNER_RETRY_BACKOFF", "100ms")
        .env("ESCUREL_RUNNER_POLL_INTERVAL", "250ms");
    let _runner = ChildGuard(cmd.spawn().expect("spawn escurel-runner"));

    // Recovery emits synthesize → the report instance appears.
    let report = await_instance(
        &gateway,
        "research-report",
        "markdown/instances/research-report/rec-synthesize-",
        45,
    )
    .await;
    assert!(
        report.ends_with(".md"),
        "recovery completed the run: {report}"
    );
}

#[tokio::test]
async fn over_budget_plan_fails_fast_at_invocation_emitting_no_steps() {
    // A plan projecting 10 runs, invoked under a max_runs_per_root of 3: the
    // up-front budget gate (§7) must refuse to start it, so NO scope step ever
    // fires and no research-angle instance appears — the run never fans out.
    let gateway = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(
            FixtureBuilder::new()
                .tenant(TENANT)
                .skill(BIG_WF_SKILL, BIG_WF_SKILL_BODY)
                .skill("research-angle", ANGLE_SKILL_BODY)
                .skill("workflow-run", RUN_SKILL_BODY)
                .done(),
        ),
        ..Default::default()
    })
    .await;

    let run_page = "markdown/instances/workflow-run/rb.md";
    call_mcp(
        &gateway,
        // A workflow invocation carries `provenance.workflow`, which the gateway
        // now accepts only from an admin/system identity (async-ops 2c-ii — a
        // non-admin caller starts a workflow via `start_operation`, not a raw
        // capture_event). These reducer/barrier tests inject the invocation as
        // that system identity to keep a fixed run-board id for their prefix
        // assertions; the facade path is covered by the start_operation tests.
        Role::Admin,
        "capture_event",
        json!({
            "source": "manual",
            "mime": "text/plain",
            "label_skill": BIG_WF_SKILL,
            "instance_page_id": run_page,
            "title": "invoke over-budget",
            "body": "too big",
            "provenance": {
                "workflow": { "run": run_page, "wf_skill": BIG_WF_SKILL, "phase": "invoke" }
            }
        }),
    )
    .await;

    // The runner authenticates as an admin identity (in production it mints its
    // own `escurel:admin` bearer): its orchestration writes include the reserved
    // `escurel:run-status` status events, which the capture guard admits only for
    // admin. (The per-run HARNESS token is the separate caller-scoped one, 2c.)
    let token = gateway.mint_token(TENANT, Role::Admin);
    let port = free_port();
    let listen = format!("127.0.0.1:{port}");
    let ledger_dir = tempfile::tempdir().expect("tempdir for ledger");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_escurel-runner"));
    cmd.env("ESCUREL_RUNNER_LISTEN", &listen)
        .env("ESCUREL_RUNNER_GATEWAY_URL", gateway.base_url())
        .env("ESCUREL_RUNNER_TENANT", TENANT)
        .env("ESCUREL_RUNNER_TOKEN", &token)
        .env("ESCUREL_RUNNER_HARNESS", "echo")
        .env(
            "ESCUREL_RUNNER_LEDGER_PATH",
            ledger_dir.path().join("ledger.sqlite").to_str().unwrap(),
        )
        // Budget of 3 < the plan's projected 10 → fail fast.
        .env("ESCUREL_RUNNER_MAX_RUNS_PER_ROOT", "3")
        .env("ESCUREL_RUNNER_MAX_ATTEMPTS", "2")
        .env("ESCUREL_RUNNER_RETRY_BACKOFF", "100ms")
        .env("ESCUREL_RUNNER_POLL_INTERVAL", "250ms");
    let _runner = ChildGuard(cmd.spawn().expect("spawn escurel-runner"));

    // Give the runner time to process the invocation and run the budget gate.
    // The invocation's run-board instance may appear (it is the invocation's
    // own confirmed write), but NO scope fan-out may follow.
    tokio::time::sleep(Duration::from_secs(6)).await;
    let angles = call_mcp(
        &gateway,
        Role::Agent,
        "list_instances",
        json!({ "skill_id": "research-angle" }),
    )
    .await;
    let count = angles["instances"].as_array().map_or(0, Vec::len);
    assert_eq!(
        count, 0,
        "over-budget plan must emit no scope steps; found {count} research-angle instances"
    );
}

// --- G1: integrative distillation (durable-target weave) -------------------

const ENTITY_SKILL_BODY: &str =
    "---\ntype: skill\nid: entity\n---\n# entity\n\nA durable entity/concept page.\n";
const ENTITY_ACME: &str = "---\ntype: instance\nskill: entity\nid: acme\n---\n# Acme Corp\n\nBaseline facts about Acme.\n";
const ENTITY_GLOBEX: &str = "---\ntype: instance\nskill: entity\nid: globex\n---\n# Globex\n\nBaseline facts about Globex.\n";

/// Poll `expand(page_id)` until its frontmatter carries `key`, returning the
/// value — or panic at the deadline.
async fn await_frontmatter_key(p: &EscurelProcess, page_id: &str, key: &str, secs: u64) -> String {
    let deadline = Instant::now() + Duration::from_secs(secs);
    loop {
        let r = call_mcp(p, Role::Agent, "expand", json!({ "page_id": page_id })).await;
        if let Some(v) = r["frontmatter"].get(key).and_then(Value::as_str) {
            return v.to_owned();
        }
        if Instant::now() >= deadline {
            panic!("{page_id} never gained frontmatter key {key} within {secs}s (got {r})");
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

/// The DoD test for compile-first-wiki **G1**: a single `distill` invocation
/// weaves into **≥2 existing entity pages** via the `writes: existing`
/// durable-target reducer branch — no mocks, real gateway + real echo harness.
///
/// Seeds two durable `entity` pages and two run-scoped `distill-claim`s (the
/// semantic `extract` output an LLM would write, each tagged with its
/// `target_page`), then invokes `distill`. The reducer's weave phase fans out
/// over the two distinct targets; each echo step folds the claim into the
/// durable page and stamps `source_event` (the completion signal); once both
/// are woven the `integrate` barrier writes the `distill-report`.
#[tokio::test]
async fn distill_weaves_one_source_into_two_existing_pages() {
    let mut tf = FixtureBuilder::new().tenant(TENANT);
    for (page_id, body) in escurel_runner_workflow::corpus::distill_corpus() {
        tf = tf.page(&page_id, body);
    }
    let acme_page = "markdown/instances/entity/acme.md";
    let globex_page = "markdown/instances/entity/globex.md";
    let run_page = "markdown/instances/workflow-run/d1.md";
    let gateway = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(
            tf.skill("entity", ENTITY_SKILL_BODY)
                .instance("entity", "acme", ENTITY_ACME)
                .instance("entity", "globex", ENTITY_GLOBEX)
                // Two run-scoped claims (extract's output), each tagged with the
                // durable page it belongs to. `d1-` prefixes them into run `d1`.
                .instance(
                    "distill-claim",
                    "d1-c-acme",
                    format!(
                        "---\ntype: instance\nskill: distill-claim\nid: d1-c-acme\n\
                         target_page: {acme_page}\naction: update\nworkflow_run: {run_page}\n\
                         ---\n# claim\n\nAcme shipped a new product line in 2026.\n"
                    ),
                )
                .instance(
                    "distill-claim",
                    "d1-c-globex",
                    format!(
                        "---\ntype: instance\nskill: distill-claim\nid: d1-c-globex\n\
                         target_page: {globex_page}\naction: update\nworkflow_run: {run_page}\n\
                         ---\n# claim\n\nGlobex opened a Berlin office in 2026.\n"
                    ),
                )
                .instance(
                    "workflow-run",
                    "d1",
                    "---\ntype: instance\nskill: workflow-run\nid: d1\nwf_skill: distill\n---\n# run d1\n",
                )
                .done(),
        ),
        ..Default::default()
    })
    .await;

    // Invoke distill: label the plan, pre-flag the run board, carry the
    // workflow provenance so the dispatch loop routes to the reducer.
    call_mcp(
        &gateway,
        // A workflow invocation carries `provenance.workflow`, which the gateway
        // now accepts only from an admin/system identity (async-ops 2c-ii — a
        // non-admin caller starts a workflow via `start_operation`, not a raw
        // capture_event). These reducer/barrier tests inject the invocation as
        // that system identity to keep a fixed run-board id for their prefix
        // assertions; the facade path is covered by the start_operation tests.
        Role::Admin,
        "capture_event",
        json!({
            "source": "manual",
            "mime": "text/plain",
            "label_skill": "distill",
            "instance_page_id": run_page,
            "title": "distill a source",
            "body": "Integrate the source's claims.",
            "provenance": { "workflow": { "run": run_page, "wf_skill": "distill", "phase": "invoke" } }
        }),
    )
    .await;

    // The runner authenticates as an admin identity (in production it mints its
    // own `escurel:admin` bearer): its orchestration writes include the reserved
    // `escurel:run-status` status events, which the capture guard admits only for
    // admin. (The per-run HARNESS token is the separate caller-scoped one, 2c.)
    let token = gateway.mint_token(TENANT, Role::Admin);
    let listen = format!("127.0.0.1:{}", free_port());
    let ledger_dir = tempfile::tempdir().expect("tempdir for ledger");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_escurel-runner"));
    cmd.env("ESCUREL_RUNNER_LISTEN", &listen)
        .env("ESCUREL_RUNNER_GATEWAY_URL", gateway.base_url())
        .env("ESCUREL_RUNNER_TENANT", TENANT)
        .env("ESCUREL_RUNNER_TOKEN", &token)
        .env("ESCUREL_RUNNER_HARNESS", "echo")
        .env(
            "ESCUREL_RUNNER_LEDGER_PATH",
            ledger_dir.path().join("ledger.sqlite").to_str().unwrap(),
        )
        .env("ESCUREL_RUNNER_MAX_DEPTH", "16")
        .env("ESCUREL_RUNNER_MAX_RUNS_PER_ROOT", "64")
        .env("ESCUREL_RUNNER_MAX_ATTEMPTS", "3")
        .env("ESCUREL_RUNNER_RETRY_BACKOFF", "100ms")
        .env("ESCUREL_RUNNER_POLL_INTERVAL", "250ms");
    let _runner = ChildGuard(cmd.spawn().expect("spawn escurel-runner"));

    // Both durable pages must gain a `source_event` stamp — proof the weave
    // touched each existing page (the width>1 breadth G1 exists for).
    let acme_src = await_frontmatter_key(&gateway, acme_page, "source_event", 45).await;
    let globex_src = await_frontmatter_key(&gateway, globex_page, "source_event", 45).await;
    assert_ne!(
        acme_src, globex_src,
        "each page carries its own weave step's event id"
    );

    // The baseline content survives (weave appends, never clobbers) and the
    // woven note is present.
    let acme = call_mcp(
        &gateway,
        Role::Agent,
        "expand",
        json!({ "page_id": acme_page }),
    )
    .await;
    let acme_body = acme["body"].as_str().unwrap_or_default();
    assert!(
        acme_body.contains("Baseline facts about Acme"),
        "baseline survived: {acme_body}"
    );
    assert!(
        acme_body.contains("folded event"),
        "weave note present: {acme_body}"
    );

    // The integrate barrier fired only after both targets were woven.
    let report = await_instance(
        &gateway,
        "distill-report",
        "markdown/instances/distill-report/d1-integrate-",
        45,
    )
    .await;
    assert!(report.ends_with(".md"), "distill-report written: {report}");
}

// --- G2: semantic lint (typed issues; proposes, never rewrites) -------------

/// Poll `list_instances(issue)` until an issue of `kind` appears; returns the
/// full issues array once the scan has recorded that kind (or panics).
async fn await_issues(p: &EscurelProcess, kind: &str, secs: u64) -> Vec<Value> {
    let deadline = Instant::now() + Duration::from_secs(secs);
    loop {
        let r = call_mcp(
            p,
            Role::Agent,
            "list_instances",
            json!({ "skill_id": "issue" }),
        )
        .await;
        let issues = r["instances"].as_array().cloned().unwrap_or_default();
        let has_kind = issues
            .iter()
            .any(|i| i["frontmatter"]["kind"].as_str() == Some(kind));
        if has_kind {
            return issues;
        }
        if Instant::now() >= deadline {
            panic!("no issue of kind {kind} within {secs}s; issues so far: {issues:?}");
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

/// The DoD test for compile-first-wiki **G2**: a `lint` run flags a seeded
/// orphan, stale page, and contradiction as typed `issue` instances — and
/// **never rewrites** the scanned pages. No mocks: real gateway + DuckDB +
/// echo harness doing real structural detection over `/mcp`.
#[tokio::test]
async fn lint_flags_orphan_stale_contradiction_without_rewriting() {
    let mut tf = FixtureBuilder::new().tenant(TENANT);
    for (page_id, body) in escurel_runner_workflow::corpus::lint_corpus() {
        tf = tf.page(&page_id, body);
    }
    let orphan_page = "markdown/instances/entity/orphan.md";
    let run_page = "markdown/instances/workflow-run/lint1.md";
    let gateway = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(
            tf.skill("entity", ENTITY_SKILL_BODY)
                .skill("note", "---\ntype: skill\nid: note\n---\n# note\n")
                // orphan: nothing links to it.
                .instance("entity", "orphan", "---\ntype: instance\nskill: entity\nid: orphan\n---\n# Orphan\n\nUnreferenced.\n")
                // stale: old last_verified, but linked (so it is stale, not orphan).
                .instance("entity", "stale", "---\ntype: instance\nskill: entity\nid: stale\nlast_verified: 2020-01-01T00:00:00Z\n---\n# Stale\n\nOld.\n")
                // contradiction: same fact_key, different fact_value; both linked.
                .instance("entity", "c1", "---\ntype: instance\nskill: entity\nid: c1\nfact_key: capital\nfact_value: Berlin\n---\n# C1\n")
                .instance("entity", "c2", "---\ntype: instance\nskill: entity\nid: c2\nfact_key: capital\nfact_value: Munich\n---\n# C2\n")
                // linked control: has an inbound link, fresh, consistent → no issue.
                .instance("entity", "linked", "---\ntype: instance\nskill: entity\nid: linked\n---\n# Linked\n")
                // The linker gives stale/c1/c2/linked an inbound edge (its own
                // skill `note` is not scanned).
                .instance("note", "links", "---\ntype: instance\nskill: note\nid: links\n---\n# links\n\nSee [[entity::stale]], [[entity::c1]], [[entity::c2]], [[entity::linked]].\n")
                // Run board carries the scan scope + staleness cutoff.
                .instance("workflow-run", "lint1", "---\ntype: instance\nskill: workflow-run\nid: lint1\nwf_skill: lint\nscan_skills: entity\nstale_before: 2025-01-01T00:00:00Z\n---\n# lint run\n")
                .done(),
        ),
        ..Default::default()
    })
    .await;

    call_mcp(
        &gateway,
        // A workflow invocation carries `provenance.workflow`, which the gateway
        // now accepts only from an admin/system identity (async-ops 2c-ii — a
        // non-admin caller starts a workflow via `start_operation`, not a raw
        // capture_event). These reducer/barrier tests inject the invocation as
        // that system identity to keep a fixed run-board id for their prefix
        // assertions; the facade path is covered by the start_operation tests.
        Role::Admin,
        "capture_event",
        json!({
            "source": "manual",
            "mime": "text/plain",
            "label_skill": "lint",
            "instance_page_id": run_page,
            "title": "invoke lint",
            "body": "Scan for health problems.",
            "provenance": { "workflow": { "run": run_page, "wf_skill": "lint", "phase": "invoke" } }
        }),
    )
    .await;

    // The runner authenticates as an admin identity (in production it mints its
    // own `escurel:admin` bearer): its orchestration writes include the reserved
    // `escurel:run-status` status events, which the capture guard admits only for
    // admin. (The per-run HARNESS token is the separate caller-scoped one, 2c.)
    let token = gateway.mint_token(TENANT, Role::Admin);
    let listen = format!("127.0.0.1:{}", free_port());
    let ledger_dir = tempfile::tempdir().expect("tempdir for ledger");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_escurel-runner"));
    cmd.env("ESCUREL_RUNNER_LISTEN", &listen)
        .env("ESCUREL_RUNNER_GATEWAY_URL", gateway.base_url())
        .env("ESCUREL_RUNNER_TENANT", TENANT)
        .env("ESCUREL_RUNNER_TOKEN", &token)
        .env("ESCUREL_RUNNER_HARNESS", "echo")
        .env(
            "ESCUREL_RUNNER_LEDGER_PATH",
            ledger_dir.path().join("ledger.sqlite").to_str().unwrap(),
        )
        .env("ESCUREL_RUNNER_MAX_DEPTH", "16")
        .env("ESCUREL_RUNNER_MAX_RUNS_PER_ROOT", "64")
        .env("ESCUREL_RUNNER_MAX_ATTEMPTS", "3")
        .env("ESCUREL_RUNNER_RETRY_BACKOFF", "100ms")
        .env("ESCUREL_RUNNER_POLL_INTERVAL", "250ms");
    let _runner = ChildGuard(cmd.spawn().expect("spawn escurel-runner"));

    // The scan records issues; wait for the summary (written last) to be sure
    // detection has completed, then inspect the full set.
    await_issues(&gateway, "lint_summary", 45).await;
    let issues = call_mcp(
        &gateway,
        Role::Agent,
        "list_instances",
        json!({ "skill_id": "issue" }),
    )
    .await;
    let issues = issues["instances"].as_array().cloned().unwrap_or_default();
    let of_kind = |kind: &str| -> Vec<String> {
        issues
            .iter()
            .filter(|i| i["frontmatter"]["kind"].as_str() == Some(kind))
            .filter_map(|i| i["frontmatter"]["subject_page"].as_str().map(str::to_owned))
            .collect()
    };

    assert_eq!(
        of_kind("orphan"),
        vec![orphan_page.to_owned()],
        "exactly the orphan is flagged"
    );
    assert_eq!(
        of_kind("stale"),
        vec!["markdown/instances/entity/stale.md".to_owned()],
        "the stale page is flagged"
    );
    let mut contradictions = of_kind("contradiction");
    contradictions.sort();
    assert_eq!(
        contradictions,
        vec![
            "markdown/instances/entity/c1.md".to_owned(),
            "markdown/instances/entity/c2.md".to_owned()
        ],
        "both sides of the contradiction are flagged"
    );

    // Lint NEVER rewrites: the scanned pages are byte-for-byte untouched — no
    // source_event stamp, original body intact.
    let orphan = call_mcp(
        &gateway,
        Role::Agent,
        "expand",
        json!({ "page_id": orphan_page }),
    )
    .await;
    assert!(
        orphan["frontmatter"].get("source_event").is_none(),
        "lint must not stamp/modify a scanned page: {orphan}"
    );
    assert_eq!(
        orphan["body"].as_str().unwrap_or_default().trim(),
        "# Orphan\n\nUnreferenced.".trim(),
        "the orphan page body is unchanged by lint"
    );
}

/// The lint **schedule** end to end: with `ESCUREL_RUNNER_LINT_INTERVAL` set,
/// the runner itself synthesizes a `lint` invocation each window; the reactive
/// loop drives it (scan config auto-discovered via `list_skills`) and an orphan
/// issue materializes — no manual invocation, gateway still automation-free.
#[tokio::test]
async fn lint_tick_schedules_a_scan_without_manual_invocation() {
    let mut tf = FixtureBuilder::new().tenant(TENANT);
    for (page_id, body) in escurel_runner_workflow::corpus::lint_corpus() {
        tf = tf.page(&page_id, body);
    }
    let gateway = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(
            tf.skill("entity", ENTITY_SKILL_BODY)
                .instance("entity", "lonely", "---\ntype: instance\nskill: entity\nid: lonely\n---\n# Lonely\n\nNo inbound links.\n")
                .done(),
        ),
        ..Default::default()
    })
    .await;

    // The runner authenticates as an admin identity (in production it mints its
    // own `escurel:admin` bearer): its orchestration writes include the reserved
    // `escurel:run-status` status events, which the capture guard admits only for
    // admin. (The per-run HARNESS token is the separate caller-scoped one, 2c.)
    let token = gateway.mint_token(TENANT, Role::Admin);
    let listen = format!("127.0.0.1:{}", free_port());
    let ledger_dir = tempfile::tempdir().expect("tempdir for ledger");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_escurel-runner"));
    cmd.env("ESCUREL_RUNNER_LISTEN", &listen)
        .env("ESCUREL_RUNNER_GATEWAY_URL", gateway.base_url())
        .env("ESCUREL_RUNNER_TENANT", TENANT)
        .env("ESCUREL_RUNNER_TOKEN", &token)
        .env("ESCUREL_RUNNER_HARNESS", "echo")
        .env(
            "ESCUREL_RUNNER_LEDGER_PATH",
            ledger_dir.path().join("ledger.sqlite").to_str().unwrap(),
        )
        .env("ESCUREL_RUNNER_MAX_DEPTH", "16")
        .env("ESCUREL_RUNNER_MAX_RUNS_PER_ROOT", "64")
        .env("ESCUREL_RUNNER_MAX_ATTEMPTS", "3")
        .env("ESCUREL_RUNNER_RETRY_BACKOFF", "100ms")
        .env("ESCUREL_RUNNER_POLL_INTERVAL", "250ms")
        // The schedule under test.
        .env("ESCUREL_RUNNER_LINT_INTERVAL", "1s");
    let _runner = ChildGuard(cmd.spawn().expect("spawn escurel-runner"));

    // No manual capture_event — the tick alone must drive a scan that flags the
    // unreferenced entity page.
    let issues = await_issues(&gateway, "orphan", 45).await;
    assert!(
        issues
            .iter()
            .any(|i| i["frontmatter"]["subject_page"].as_str()
                == Some("markdown/instances/entity/lonely.md")),
        "the scheduled scan flagged the orphan: {issues:?}"
    );
}

// --- G3: freshness + curated index -----------------------------------------

/// Spawn the real runner (echo harness) against `gateway` and return its guard.
fn spawn_echo_runner(gateway: &EscurelProcess, ledger_dir: &std::path::Path) -> ChildGuard {
    // The runner authenticates as an admin identity (in production it mints its
    // own `escurel:admin` bearer): its orchestration writes include the reserved
    // `escurel:run-status` status events, which the capture guard admits only for
    // admin. (The per-run HARNESS token is the separate caller-scoped one, 2c.)
    let token = gateway.mint_token(TENANT, Role::Admin);
    let listen = format!("127.0.0.1:{}", free_port());
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_escurel-runner"));
    cmd.env("ESCUREL_RUNNER_LISTEN", &listen)
        .env("ESCUREL_RUNNER_GATEWAY_URL", gateway.base_url())
        .env("ESCUREL_RUNNER_TENANT", TENANT)
        .env("ESCUREL_RUNNER_TOKEN", &token)
        .env("ESCUREL_RUNNER_HARNESS", "echo")
        .env(
            "ESCUREL_RUNNER_LEDGER_PATH",
            ledger_dir.join("ledger.sqlite").to_str().unwrap(),
        )
        .env("ESCUREL_RUNNER_MAX_DEPTH", "16")
        .env("ESCUREL_RUNNER_MAX_RUNS_PER_ROOT", "64")
        .env("ESCUREL_RUNNER_MAX_ATTEMPTS", "3")
        .env("ESCUREL_RUNNER_RETRY_BACKOFF", "100ms")
        .env("ESCUREL_RUNNER_POLL_INTERVAL", "250ms");
    ChildGuard(cmd.spawn().expect("spawn escurel-runner"))
}

async fn invoke_curate(gateway: &EscurelProcess, run_page: &str) {
    call_mcp(
        gateway,
        // Workflow invocation → admin/system identity (async-ops 2c-ii;
        // `provenance.workflow` is server-owned for non-admin callers).
        Role::Admin,
        "capture_event",
        json!({
            "source": "manual",
            "mime": "text/plain",
            "label_skill": "curate",
            "instance_page_id": run_page,
            "title": "invoke curate",
            "body": "Regenerate the index.",
            "provenance": { "workflow": { "run": run_page, "wf_skill": "curate", "phase": "invoke" } }
        }),
    )
    .await;
}

/// The DoD test for compile-first-wiki **G3**: `curate` regenerates a
/// by-category `index` instance (the map of the territory), and it stays
/// **derivable** — re-running over the same corpus reproduces the same body.
/// No mocks: real gateway + DuckDB + echo harness.
#[tokio::test]
async fn curate_generates_a_derivable_by_category_index() {
    let mut tf = FixtureBuilder::new().tenant(TENANT);
    for (page_id, body) in escurel_runner_workflow::corpus::curation_corpus() {
        tf = tf.page(&page_id, body);
    }
    let gateway = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(
            tf.skill("entity", ENTITY_SKILL_BODY)
                .instance("entity", "acme", ENTITY_ACME)
                .instance("entity", "globex", ENTITY_GLOBEX)
                .done(),
        ),
        ..Default::default()
    })
    .await;

    let ledger_dir = tempfile::tempdir().expect("tempdir for ledger");
    let _runner = spawn_echo_runner(&gateway, ledger_dir.path());

    // First curation.
    invoke_curate(&gateway, "markdown/instances/workflow-run/cur1.md").await;
    let idx1 = await_instance(&gateway, "index", "markdown/instances/index/cur1-", 45).await;
    let expanded1 = call_mcp(&gateway, Role::Agent, "expand", json!({ "page_id": idx1 })).await;
    let body1 = expanded1["body"].as_str().unwrap_or_default().to_owned();

    // The map lists the entity category and both instances, with a generated_at
    // freshness stamp.
    assert!(
        body1.contains("## entity"),
        "index groups by category: {body1}"
    );
    assert!(body1.contains("[[entity::acme]]"), "lists acme: {body1}");
    assert!(
        body1.contains("[[entity::globex]]"),
        "lists globex: {body1}"
    );
    assert!(
        expanded1["frontmatter"]["generated_at"].as_str().is_some(),
        "index carries a generated_at stamp"
    );

    // Derivable: a second curation over the same corpus reproduces the same
    // body (a pure function of pages/ + events).
    invoke_curate(&gateway, "markdown/instances/workflow-run/cur2.md").await;
    let idx2 = await_instance(&gateway, "index", "markdown/instances/index/cur2-", 45).await;
    let expanded2 = call_mcp(&gateway, Role::Agent, "expand", json!({ "page_id": idx2 })).await;
    let body2 = expanded2["body"].as_str().unwrap_or_default().to_owned();
    assert_eq!(
        body1, body2,
        "the index is derivable — same corpus, same map"
    );
}

/// **G3 freshness**: a distilled page gains a `last_verified` stamp so lint's
/// staleness check has something to read.
#[tokio::test]
async fn distill_stamps_last_verified_on_the_woven_page() {
    let mut tf = FixtureBuilder::new().tenant(TENANT);
    for (page_id, body) in escurel_runner_workflow::corpus::distill_corpus() {
        tf = tf.page(&page_id, body);
    }
    let target = "markdown/instances/entity/acme.md";
    let run_page = "markdown/instances/workflow-run/f1.md";
    let gateway = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(
            tf.skill("entity", ENTITY_SKILL_BODY)
                .instance("entity", "acme", ENTITY_ACME)
                .instance(
                    "distill-claim",
                    "f1-c-acme",
                    format!("---\ntype: instance\nskill: distill-claim\nid: f1-c-acme\ntarget_page: {target}\naction: update\n---\n# claim\n\nAcme fact.\n"),
                )
                .instance("workflow-run", "f1", "---\ntype: instance\nskill: workflow-run\nid: f1\nwf_skill: distill\n---\n# run\n")
                .done(),
        ),
        ..Default::default()
    })
    .await;

    call_mcp(
        &gateway,
        // A workflow invocation carries `provenance.workflow`, which the gateway
        // now accepts only from an admin/system identity (async-ops 2c-ii — a
        // non-admin caller starts a workflow via `start_operation`, not a raw
        // capture_event). These reducer/barrier tests inject the invocation as
        // that system identity to keep a fixed run-board id for their prefix
        // assertions; the facade path is covered by the start_operation tests.
        Role::Admin,
        "capture_event",
        json!({
            "source": "manual", "mime": "text/plain", "label_skill": "distill",
            "instance_page_id": run_page, "title": "distill", "body": "go",
            "provenance": { "workflow": { "run": run_page, "wf_skill": "distill", "phase": "invoke" } }
        }),
    )
    .await;

    let ledger_dir = tempfile::tempdir().expect("tempdir for ledger");
    let _runner = spawn_echo_runner(&gateway, ledger_dir.path());

    let lv = await_frontmatter_key(&gateway, target, "last_verified", 45).await;
    assert!(
        !lv.is_empty(),
        "woven page gained a last_verified freshness stamp"
    );
}

// --- G4: eval-driven improvement (improve documents AND skills) -------------

/// Invoke the `eval` workflow for `run_page` (scores tasks, then weaves fixes).
async fn invoke_eval(gateway: &EscurelProcess, run_page: &str) {
    call_mcp(
        gateway,
        // Workflow invocation → admin/system identity (async-ops 2c-ii;
        // `provenance.workflow` is server-owned for non-admin callers).
        Role::Admin,
        "capture_event",
        json!({
            "source": "manual", "mime": "text/plain", "label_skill": "eval",
            "instance_page_id": run_page, "title": "invoke eval", "body": "Score and improve.",
            "provenance": { "workflow": { "run": run_page, "wf_skill": "eval", "phase": "invoke" } }
        }),
    )
    .await;
}

/// Whether an eval-result for `task_id` with `verdict` exists (results
/// accumulate across runs, so we check for existence, not "the first").
async fn eval_result_exists(gateway: &EscurelProcess, task_id: &str, verdict: &str) -> bool {
    let r = call_mcp(
        gateway,
        Role::Agent,
        "list_instances",
        json!({ "skill_id": "eval-result" }),
    )
    .await;
    r["instances"].as_array().is_some_and(|a| {
        a.iter().any(|i| {
            i["frontmatter"]["task"].as_str() == Some(task_id)
                && i["frontmatter"]["verdict"].as_str() == Some(verdict)
        })
    })
}

/// The DoD test for compile-first-wiki **G4**: an `eval` run detects a failing
/// task, weaves the fix into the implicated **skill** (improving the connective
/// tissue, not just an instance), and a re-run confirms the task now passes.
/// No mocks: real gateway + DuckDB + echo harness doing real structural
/// scoring + the G1 durable-target weave.
#[tokio::test]
async fn eval_improves_a_failing_skill_then_reverify_passes() {
    let mut tf = FixtureBuilder::new().tenant(TENANT);
    for (page_id, body) in escurel_runner_workflow::corpus::eval_corpus() {
        tf = tf.page(&page_id, body);
    }
    let faq_skill = "markdown/skills/faq.md";
    let gateway = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(
            // The skill under evaluation — initially missing the expected fact.
            tf.page("skills/faq.md", "---\ntype: skill\nid: faq\ndescription: FAQ\n---\n# FAQ\n\nEscurel is a knowledge base.\n")
                // A persistent benchmark task (not run-scoped) — every eval run
                // scores it; the fix is applied once, then re-scoring passes.
                .instance("eval-task", "air", format!("---\ntype: instance\nskill: eval-task\nid: air\nimplicated_page: {faq_skill}\nexpect: air-gappable\nfix: Escurel is fully air-gappable.\n---\n# task\n"))
                .done(),
        ),
        ..Default::default()
    })
    .await;

    let ledger_dir = tempfile::tempdir().expect("tempdir for ledger");
    let _runner = spawn_echo_runner(&gateway, ledger_dir.path());

    // Run 1: score (fail) → apply weaves the fix into the FAQ skill.
    invoke_eval(&gateway, "markdown/instances/workflow-run/ev1.md").await;
    // The skill gains the expected content + a source_event (proof it was
    // edited — a skill, the connective tissue, not just a data instance).
    let src = await_frontmatter_key(&gateway, faq_skill, "source_event", 45).await;
    assert!(
        !src.is_empty(),
        "the skill was improved (source_event stamped)"
    );
    let faq = call_mcp(
        &gateway,
        Role::Agent,
        "expand",
        json!({ "page_id": faq_skill }),
    )
    .await;
    assert!(
        faq["body"]
            .as_str()
            .unwrap_or_default()
            .contains("air-gappable"),
        "the fix was woven into the skill: {}",
        faq["body"]
    );
    // The skill identity survived the edit (still a skill named faq).
    assert_eq!(faq["frontmatter"]["id"].as_str(), Some("faq"));

    // Run 2: re-verify — the task now PASSES (the improvement held), and no
    // eval_regression issue is raised.
    invoke_eval(&gateway, "markdown/instances/workflow-run/ev2.md").await;
    let deadline = Instant::now() + Duration::from_secs(45);
    loop {
        if eval_result_exists(&gateway, "air", "pass").await {
            break;
        }
        assert!(Instant::now() < deadline, "re-eval never passed");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    let issues = call_mcp(
        &gateway,
        Role::Agent,
        "list_instances",
        json!({ "skill_id": "issue" }),
    )
    .await;
    let regressions = issues["instances"].as_array().map_or(0, |a| {
        a.iter()
            .filter(|i| i["frontmatter"]["kind"].as_str() == Some("eval_regression"))
            .count()
    });
    assert_eq!(
        regressions, 0,
        "a held improvement raises no eval_regression"
    );
}

/// **G4 bounded loop**: when the fix does NOT resolve the task, a re-eval on the
/// already-improved page raises an `eval_regression` issue for human review
/// rather than looping forever.
#[tokio::test]
async fn eval_regression_is_flagged_when_a_fix_does_not_hold() {
    let mut tf = FixtureBuilder::new().tenant(TENANT);
    for (page_id, body) in escurel_runner_workflow::corpus::eval_corpus() {
        tf = tf.page(&page_id, body);
    }
    let doc = "markdown/skills/faq.md";
    let gateway = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(
            tf.page("skills/faq.md", "---\ntype: skill\nid: faq\ndescription: FAQ\n---\n# FAQ\n\nEscurel is a KB.\n")
                // A BROKEN fix: the woven text does not contain the expected string.
                .instance("eval-task", "x", format!("---\ntype: instance\nskill: eval-task\nid: x\nimplicated_page: {doc}\nexpect: air-gappable\nfix: This note does not answer it.\n---\n# task\n"))
                .done(),
        ),
        ..Default::default()
    })
    .await;

    let ledger_dir = tempfile::tempdir().expect("tempdir for ledger");
    let _runner = spawn_echo_runner(&gateway, ledger_dir.path());

    // Run 1: fail → apply weaves the (broken) fix, stamping source_event.
    invoke_eval(&gateway, "markdown/instances/workflow-run/rg1.md").await;
    await_frontmatter_key(&gateway, doc, "source_event", 45).await;

    // Run 2: the task still fails on an already-improved page → eval_regression.
    invoke_eval(&gateway, "markdown/instances/workflow-run/rg2.md").await;
    let issues = await_issues(&gateway, "eval_regression", 45).await;
    assert!(
        issues
            .iter()
            .any(|i| i["frontmatter"]["subject_page"].as_str() == Some(doc)),
        "an unresolved fix raises an eval_regression for the page: {issues:?}"
    );
}

/// A stub agent A2A endpoint that completes any delegated task immediately with
/// the given `result_ref` in `task.metadata.result_ref` (JSON-RPC `message/send`
/// and `tasks/get` both answer `completed`).
async fn spawn_stub_delegate_agent(result_ref: Value) -> String {
    use axum::{Router, routing::post};
    let task = json!({
        "id": "task-e2e",
        "status": { "state": "completed" },
        "metadata": { "result_ref": result_ref },
    });
    let app = Router::new().route(
        "/a2a",
        post(move |_body: String| {
            let task = task.clone();
            async move { axum::Json(json!({ "jsonrpc": "2.0", "id": 1, "result": task })) }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    format!("http://{addr}/a2a")
}

/// async-ops Phase 4 (crew option D), the full loop end-to-end: a real gateway +
/// a MINTING runner delegate a `harness: delegate` step to a stub agent, the
/// agent returns a `result_ref`, the harness SEALS it (writes the step's produced
/// instance over /mcp with the requester's scoped token), the reducer confirms
/// the step, and `get_operation` surfaces the `result_ref`. This is the
/// integration proof that the delegate seal closes the loop.
#[tokio::test]
async fn a_delegate_step_seals_and_get_operation_surfaces_the_result_ref_end_to_end() {
    let gateway = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(
            FixtureBuilder::new()
                .tenant(TENANT)
                .skill(DELEGATE_WF_SKILL, DELEGATE_WF_BODY)
                .skill("research-report", REPORT_SKILL_BODY)
                .skill("workflow-run", RUN_SKILL_BODY)
                .done(),
        ),
        ..Default::default()
    })
    .await;

    let result_ref = json!({ "kind": "scenario_parquet", "scenario_id": "scn-delegate-e2e" });
    let agent = spawn_stub_delegate_agent(result_ref.clone()).await;

    // The requester starts the operation via the facade, so the run board carries
    // `requested_by` — the minting runner scopes the run to it, and the seal
    // writes the produced instance as that requester (the #468 produced-write grant).
    let requester = "alice-requester";
    let started = call_mcp_as(
        &gateway,
        Role::Agent,
        requester,
        "start_operation",
        json!({ "wf_skill": DELEGATE_WF_SKILL, "input": "Run the delegated step." }),
    )
    .await;
    let operation_id = started["operation_id"]
        .as_str()
        .expect("operation_id")
        .to_owned();

    // A MINTING runner (so it can mint the per-run scoped token AND the delegation
    // token) pointed at the stub agent.
    let (signing_key, kid) = gateway.signing_material();
    let issuer = gateway.issuer_url().to_owned();
    let port = free_port();
    let ledger_dir = tempfile::tempdir().expect("tempdir for ledger");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_escurel-runner"));
    cmd.env("ESCUREL_RUNNER_LISTEN", format!("127.0.0.1:{port}"))
        .env("ESCUREL_RUNNER_GATEWAY_URL", gateway.base_url())
        .env("ESCUREL_RUNNER_TENANT", TENANT)
        .env_remove("ESCUREL_RUNNER_TOKEN")
        .env("ESCUREL_RUNNER_AUTH_ISSUER", &issuer)
        .env("ESCUREL_RUNNER_AUTH_KID", kid)
        .env("ESCUREL_RUNNER_AUTH_SIGNING_KEY", &signing_key)
        .env("ESCUREL_RUNNER_AUTH_SUBJECT", "escurel-runner")
        .env("ESCUREL_RUNNER_HARNESS", "echo") // the plan's step declares `delegate`
        .env("ESCUREL_RUNNER_AGENT_A2A_URL", &agent)
        .env("ESCUREL_RUNNER_AGENT_A2A_AUDIENCE", "agent-a2a")
        .env(
            "ESCUREL_RUNNER_LEDGER_PATH",
            ledger_dir.path().join("ledger.sqlite").to_str().unwrap(),
        )
        .env("ESCUREL_RUNNER_MAX_DEPTH", "16")
        .env("ESCUREL_RUNNER_MAX_RUNS_PER_ROOT", "64")
        .env("ESCUREL_RUNNER_MAX_ATTEMPTS", "3")
        .env("ESCUREL_RUNNER_RETRY_BACKOFF", "100ms")
        .env("ESCUREL_RUNNER_POLL_INTERVAL", "250ms");
    let _runner = ChildGuard(cmd.spawn().expect("spawn escurel-runner"));

    // The delegate step seals its result → the reducer confirms → the operation
    // reaches terminal `succeeded`.
    assert!(
        await_operation_status(&gateway, &operation_id, "succeeded", 60).await,
        "the delegate operation must reach `succeeded` (seal → confirm → done)"
    );

    // get_operation surfaces the agent's result_ref, carried by the sealed step.
    let op = call_mcp_as(
        &gateway,
        Role::Agent,
        requester,
        "get_operation",
        json!({ "operation_id": operation_id }),
    )
    .await;
    assert_eq!(op["status"], json!("succeeded"), "{op}");
    assert_eq!(
        op["result_ref"], result_ref,
        "the delegate result_ref must ride through the seal to get_operation: {op}"
    );
}

/// No-mock (fleet #801, option D — the delivery half): a delegated operation
/// started from a chat turn delivers its produced `result_ref` on the terminal
/// `/v1/outbound` callback, so the agent's receiver can resolve + render the
/// table into the reply. Without this the callback carried only the status and a
/// delegated op's chat reply was a bare "succeeded".
#[tokio::test]
async fn a_delegate_operation_delivers_its_result_ref_on_the_terminal_callback() {
    let gateway = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(
            FixtureBuilder::new()
                .tenant(TENANT)
                .skill(DELEGATE_WF_SKILL, DELEGATE_WF_BODY)
                .skill("research-report", REPORT_SKILL_BODY)
                .skill("workflow-run", RUN_SKILL_BODY)
                .done(),
        ),
        ..Default::default()
    })
    .await;

    let result_ref = json!({ "kind": "scenario_parquet", "scenario_id": "scn-delivery-e2e" });
    let agent = spawn_stub_delegate_agent(result_ref.clone()).await;
    let (sink_url, received) = spawn_outbound_sink().await;

    // A chat turn: start with a conversation reference + the channel's tenant.
    let conversation_ref = json!({
        "channel": "msteams",
        "conversation": { "id": "19:deleg_thread@thread.v2" },
        "service_url": "https://smba.example/teams"
    });
    let requester = "alice-requester";
    let started = call_mcp_as(
        &gateway,
        Role::Agent,
        requester,
        "start_operation",
        json!({
            "wf_skill": DELEGATE_WF_SKILL,
            "input": "Run the delegated step.",
            "conversation_ref": conversation_ref,
            "channel_tenant": "acme-tenant-guid",
        }),
    )
    .await;
    let operation_id = started["operation_id"]
        .as_str()
        .expect("operation_id")
        .to_owned();

    // A MINTING runner (per-run scoped token + the delegation token) wired to the
    // stub agent AND the outbound sink.
    let (signing_key, kid) = gateway.signing_material();
    let issuer = gateway.issuer_url().to_owned();
    let port = free_port();
    let ledger_dir = tempfile::tempdir().expect("tempdir for ledger");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_escurel-runner"));
    cmd.env("ESCUREL_RUNNER_LISTEN", format!("127.0.0.1:{port}"))
        .env("ESCUREL_RUNNER_GATEWAY_URL", gateway.base_url())
        .env("ESCUREL_RUNNER_TENANT", TENANT)
        .env_remove("ESCUREL_RUNNER_TOKEN")
        .env("ESCUREL_RUNNER_AUTH_ISSUER", &issuer)
        .env("ESCUREL_RUNNER_AUTH_KID", kid)
        .env("ESCUREL_RUNNER_AUTH_SIGNING_KEY", &signing_key)
        .env("ESCUREL_RUNNER_AUTH_SUBJECT", "escurel-runner")
        .env("ESCUREL_RUNNER_HARNESS", "echo")
        .env("ESCUREL_RUNNER_AGENT_A2A_URL", &agent)
        .env("ESCUREL_RUNNER_AGENT_A2A_AUDIENCE", "agent-a2a")
        .env("ESCUREL_RUNNER_OUTBOUND_URL", &sink_url)
        .env(
            "ESCUREL_RUNNER_LEDGER_PATH",
            ledger_dir.path().join("ledger.sqlite").to_str().unwrap(),
        )
        .env("ESCUREL_RUNNER_MAX_DEPTH", "16")
        .env("ESCUREL_RUNNER_MAX_RUNS_PER_ROOT", "64")
        .env("ESCUREL_RUNNER_MAX_ATTEMPTS", "3")
        .env("ESCUREL_RUNNER_RETRY_BACKOFF", "100ms")
        .env("ESCUREL_RUNNER_POLL_INTERVAL", "250ms");
    let _runner = ChildGuard(cmd.spawn().expect("spawn escurel-runner"));

    // The terminal delivery carries the produced result_ref (find it among any
    // progress pushes).
    let deadline = Instant::now() + Duration::from_secs(60);
    let delivered = loop {
        let hit = received
            .lock()
            .expect("sink mutex")
            .iter()
            .find(|d| {
                d["operation_id"].as_str() == Some(operation_id.as_str())
                    && d["status"].as_str() == Some("succeeded")
            })
            .cloned();
        if let Some(d) = hit {
            break Some(d);
        }
        if Instant::now() >= deadline {
            break None;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    };
    let delivery = delivered.expect("the delegate operation must be delivered to the courier");
    assert_eq!(
        delivery["result_ref"], result_ref,
        "the produced result_ref must ride the terminal callback for the agent to render: {delivery}"
    );
}
