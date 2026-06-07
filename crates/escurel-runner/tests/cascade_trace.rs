//! DoD test (b) for issue #158 — **one trace per cascade lineage**, with **no
//! mocks**.
//!
//! Per the issue's Scope/DoD: run ONE real cascade and assert a single real
//! OTel trace spans all hops — i.e. all hops share one `trace_id`.
//!
//! ## The real signal we assert (and why)
//!
//! `escurel-obs` exposes an OTLP *export* pipeline but no in-memory
//! `SpanExporter` handle a test can read back without standing up a collector.
//! Rather than over-engineer a collector, we assert the **stronger, fully
//! real** signal the issue explicitly offers as the robust no-mock choice: the
//! `trace_id` the runner carries in `provenance.runner` is **identical across
//! every cascaded hop**, read straight back from the REAL gateway. The runner
//! mints one trace id on the root run's span and stamps it into each cascaded
//! event's `provenance.runner.trace_id`; the next hop reads it back and
//! continues the SAME trace (its root span uses that id). Identical trace ids
//! across the hops' events — read from the real `/mcp` surface — is the real,
//! no-mock proof that one trace spans the whole lineage.
//!
//! ## The real multi-hop cascade
//!
//! We reuse the cross-skill A→B→A chain (the #157 cycle fixture): each skill
//! folds into the OTHER skill's instance, so every confirmed write cascades a
//! follow-on event of the other skill. With a generous depth cap the chain
//! produces SEVERAL cascaded hops before a loop control stops it — every one
//! sharing the original root, hence the one trace id. The test is
//! deadline-bounded; the loop controls guarantee termination.

use std::collections::HashSet;
use std::net::TcpListener;
use std::process::{Child, Command};
use std::time::{Duration, Instant};

use escurel_test_support::{AuthMode, EscurelProcess, FixtureBuilder, Opts, Role};
use serde_json::{Value, json};

const TENANT: &str = "acme";
const ALPHA_SKILL: &str = "alpha";
const BETA_SKILL: &str = "beta";

const ALPHA_SKILL_BODY: &str = "---\ntype: skill\nid: alpha\ncascade_target: markdown/instances/beta/b1.md\n---\n# alpha\n\nFold the event into the beta instance.\n";
const BETA_SKILL_BODY: &str = "---\ntype: skill\nid: beta\ncascade_target: markdown/instances/alpha/a1.md\n---\n# beta\n\nFold the event into the alpha instance.\n";
const A_INSTANCE_BODY: &str =
    "---\ntype: instance\nid: a1\nskill: alpha\n---\n# A1\n\nBASELINE alpha.\n";
const B_INSTANCE_BODY: &str =
    "---\ntype: instance\nid: b1\nskill: beta\n---\n# B1\n\nBASELINE beta.\n";

struct ChildGuard(Child);
impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn free_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
    listener.local_addr().expect("read local_addr").port()
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
    body["result"].clone()
}

#[tokio::test]
async fn one_trace_id_spans_all_cascade_hops() {
    // 1. Real gateway with the cross-skill A→B→A chain.
    let gateway = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(
            FixtureBuilder::new()
                .tenant(TENANT)
                .skill(ALPHA_SKILL, ALPHA_SKILL_BODY)
                .skill(BETA_SKILL, BETA_SKILL_BODY)
                .instance(ALPHA_SKILL, "a1", A_INSTANCE_BODY)
                .instance(BETA_SKILL, "b1", B_INSTANCE_BODY)
                .done(),
        ),
        ..Default::default()
    })
    .await;

    // 2. Seed E0: an alpha event pre-flagged to the beta instance — cross-skill,
    //    so it cascades, and each hop cascades again until a control stops it.
    let captured = call_mcp(
        &gateway,
        Role::Agent,
        "capture_event",
        json!({
            "source": "manual",
            "mime": "text/plain",
            "label_skill": ALPHA_SKILL,
            "instance_page_id": "markdown/instances/beta/b1.md",
            "title": "seed",
            "body": "kick off the cascade"
        }),
    )
    .await;
    let seed_event_id = captured["event_id"].as_str().unwrap().to_owned();

    // 3. Real runner with a generous depth so SEVERAL hops fire (all sharing
    //    the root), with the budget as the eventual backstop.
    let token = gateway.mint_token(TENANT, Role::Agent);
    let port = free_port();
    let listen = format!("127.0.0.1:{port}");
    let ledger_dir = tempfile::tempdir().expect("tempdir for ledger");
    let ledger_path = ledger_dir.path().join("ledger.sqlite");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_escurel-runner"));
    cmd.env("ESCUREL_RUNNER_LISTEN", &listen)
        .env("ESCUREL_RUNNER_GATEWAY_URL", gateway.base_url())
        .env("ESCUREL_RUNNER_TENANT", TENANT)
        .env("ESCUREL_RUNNER_TOKEN", &token)
        .env("ESCUREL_RUNNER_HARNESS", "echo")
        .env("ESCUREL_RUNNER_LEDGER_PATH", ledger_path.to_str().unwrap())
        .env("ESCUREL_RUNNER_MAX_DEPTH", "4")
        .env("ESCUREL_RUNNER_MAX_RUNS_PER_ROOT", "16")
        .env("ESCUREL_RUNNER_MAX_ATTEMPTS", "2")
        .env("ESCUREL_RUNNER_RETRY_BACKOFF", "80ms")
        .env("ESCUREL_RUNNER_POLL_INTERVAL", "150ms");
    let _runner = ChildGuard(cmd.spawn().expect("spawn escurel-runner"));

    // 4. Accumulate, across the cascade, every cascaded hop's trace_id keyed by
    //    its event id (a hop transits the inbox then binds to an instance, so
    //    we sweep BOTH the live inbox and each instance's event history — the
    //    real gateway surfaces the provenance both places). Stop once at least
    //    two distinct cascaded hops have carried a trace id.
    use std::collections::HashMap;
    let mut by_event: HashMap<String, String> = HashMap::new();
    let collect = |events: &[Value], acc: &mut HashMap<String, String>| {
        for e in events {
            let runner = &e["provenance"]["runner"];
            if runner.is_object()
                && runner["root_event_id"].as_str() == Some(seed_event_id.as_str())
                && let Some(tid) = runner["trace_id"].as_str()
                && let Some(eid) = e["event_id"].as_str()
            {
                acc.insert(eid.to_owned(), tid.to_owned());
            }
        }
    };
    let deadline = Instant::now() + Duration::from_secs(60);
    let trace_ids = loop {
        let inbox = call_mcp(&gateway, Role::Agent, "list_inbox", json!({ "limit": 200 })).await;
        if let Some(events) = inbox["events"].as_array() {
            collect(events, &mut by_event);
        }
        for inst in [
            "markdown/instances/alpha/a1.md",
            "markdown/instances/beta/b1.md",
        ] {
            let ev = call_mcp(
                &gateway,
                Role::Agent,
                "list_events",
                json!({ "instance_page_id": inst, "limit": 200 }),
            )
            .await;
            if let Some(events) = ev["events"].as_array() {
                collect(events, &mut by_event);
            }
        }
        // At least two distinct cascaded hops carrying a trace id → enough to
        // assert "one trace spans the hops".
        if by_event.len() >= 2 {
            break by_event.values().cloned().collect::<Vec<_>>();
        }
        if Instant::now() >= deadline {
            panic!(
                "fewer than two cascaded hops carried a trace_id within the deadline (saw {})",
                by_event.len()
            );
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    };

    // 5. All hops share ONE trace id (a single trace spans the lineage), and it
    //    is a real, non-empty 32-hex W3C-style id.
    let unique: HashSet<&str> = trace_ids.iter().map(String::as_str).collect();
    assert_eq!(
        unique.len(),
        1,
        "every cascaded hop must share one trace id; saw {trace_ids:?}"
    );
    let trace_id = trace_ids[0].as_str();
    assert_eq!(
        trace_id.len(),
        32,
        "trace id is a 128-bit hex id: {trace_id}"
    );
    assert!(
        trace_id.chars().all(|c| c.is_ascii_hexdigit()),
        "trace id must be hex: {trace_id}"
    );
}
