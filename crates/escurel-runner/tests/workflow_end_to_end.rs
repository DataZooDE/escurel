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
const RUN_SKILL_BODY: &str =
    "---\ntype: skill\nid: workflow-run\n---\n# workflow-run\n\nThe run board.\n";

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
    //    dispatch loop routes the confirmed write to the reducer.
    let run_page = "markdown/instances/workflow-run/r1.md";
    call_mcp(
        &gateway,
        Role::Agent,
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
    let token = gateway.mint_token(TENANT, Role::Agent);
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
        Role::Agent,
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

    let token = gateway.mint_token(TENANT, Role::Agent);
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
/// on GEMINI_API_KEY, like the other `*_live` adapters). The `scripts/
/// gemini-workflow-runner.py` runner speaks the ADK adapter contract; each
/// phase's instance body is authored by Gemini over the real `/mcp` surface.
/// Run with:  GEMINI_API_KEY=… cargo test -p escurel-runner --test
/// workflow_end_to_end deep_research_runs_against_gemini -- --nocapture
#[tokio::test]
async fn deep_research_runs_against_gemini() {
    if std::env::var("GEMINI_API_KEY").is_err() {
        eprintln!("skipping: GEMINI_API_KEY not set");
        return;
    }
    let script = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../scripts/gemini-workflow-runner.py"
    );

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
        Role::Agent,
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

    let token = gateway.mint_token(TENANT, Role::Agent);
    let port = free_port();
    let listen = format!("127.0.0.1:{port}");
    let ledger_dir = tempfile::tempdir().expect("tempdir for ledger");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_escurel-runner"));
    cmd.env("ESCUREL_RUNNER_LISTEN", &listen)
        .env("ESCUREL_RUNNER_GATEWAY_URL", gateway.base_url())
        .env("ESCUREL_RUNNER_TENANT", TENANT)
        .env("ESCUREL_RUNNER_TOKEN", &token)
        .env("ESCUREL_RUNNER_HARNESS", "adk")
        .env("ESCUREL_RUNNER_ADK_BIN", script)
        .env("ESCUREL_RUNNER_ADK_MODEL", "gemini-2.5-flash")
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
    let script = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../scripts/gemini-workflow-runner.py"
    );

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
        Role::Agent,
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

    let token = gateway.mint_token(TENANT, Role::Agent);
    let port = free_port();
    let listen = format!("127.0.0.1:{port}");
    let ledger_dir = tempfile::tempdir().expect("tempdir for ledger");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_escurel-runner"));
    cmd.env("ESCUREL_RUNNER_LISTEN", &listen)
        .env("ESCUREL_RUNNER_GATEWAY_URL", gateway.base_url())
        .env("ESCUREL_RUNNER_TENANT", TENANT)
        .env("ESCUREL_RUNNER_TOKEN", &token)
        .env("ESCUREL_RUNNER_HARNESS", "adk")
        .env("ESCUREL_RUNNER_ADK_BIN", script)
        .env("ESCUREL_RUNNER_ADK_MODEL", "gemini-2.5-flash")
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
    let token = gateway.mint_token(TENANT, Role::Agent);
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
        Role::Agent,
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

    let token = gateway.mint_token(TENANT, Role::Agent);
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
