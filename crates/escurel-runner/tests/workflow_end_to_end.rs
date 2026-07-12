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

// --- G1: integrative distillation (durable-target weave) -------------------

const ENTITY_SKILL_BODY: &str =
    "---\ntype: skill\nid: entity\n---\n# entity\n\nA durable entity/concept page.\n";
const ENTITY_ACME: &str =
    "---\ntype: instance\nskill: entity\nid: acme\n---\n# Acme Corp\n\nBaseline facts about Acme.\n";
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
        Role::Agent,
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

    let token = gateway.mint_token(TENANT, Role::Agent);
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
    assert!(acme_body.contains("Baseline facts about Acme"), "baseline survived: {acme_body}");
    assert!(acme_body.contains("folded event"), "weave note present: {acme_body}");

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
        let r = call_mcp(p, Role::Agent, "list_instances", json!({ "skill_id": "issue" })).await;
        let issues = r["instances"].as_array().cloned().unwrap_or_default();
        let has_kind = issues.iter().any(|i| {
            i["frontmatter"]["kind"].as_str() == Some(kind)
        });
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
        Role::Agent,
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

    let token = gateway.mint_token(TENANT, Role::Agent);
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

    assert_eq!(of_kind("orphan"), vec![orphan_page.to_owned()], "exactly the orphan is flagged");
    assert_eq!(of_kind("stale"), vec!["markdown/instances/entity/stale.md".to_owned()], "the stale page is flagged");
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
    let orphan = call_mcp(&gateway, Role::Agent, "expand", json!({ "page_id": orphan_page })).await;
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

    let token = gateway.mint_token(TENANT, Role::Agent);
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
        issues.iter().any(|i| i["frontmatter"]["subject_page"].as_str()
            == Some("markdown/instances/entity/lonely.md")),
        "the scheduled scan flagged the orphan: {issues:?}"
    );
}
