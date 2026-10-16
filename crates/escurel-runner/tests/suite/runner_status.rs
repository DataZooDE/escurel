//! The runner reports its own health as ONE overwritten record
//! (`runner-status:<runner_id>`, label `escurel:runner-status`, via
//! `report_runner_status`): refreshed every `ESCUREL_RUNNER_STATUS_INTERVAL`,
//! and at once when what it reports changes (a run starts or ends, a tenant
//! pauses). The workbench reads it with `list_events { label_skill,
//! newest_first: true, limit: 1 }`. A gateway that does not know the tool
//! (an older image) answers `-32601`; the runner then falls back, once, to the
//! heartbeat-event stream it used before.
//!
//! Real gateway, real runner binary, real echo harness idling so a run is
//! observably live.

use std::process::{Child, Command};
use std::time::{Duration, Instant};

use escurel_test_support::{AuthMode, EscurelProcess, FixtureBuilder, Opts, Role, free_port};
use serde_json::{Value, json};

const TENANT: &str = "acme";
const SKILL_BODY: &str = "---\nkind: skill\nid: renewal\nautonomy: auto\n---\n# renewal\n\nFold the event into the instance.\n";
const INSTANCE_BODY: &str = "---\nkind: instance\nid: c1\nskill: renewal\n---\n# C1\n\nBASELINE.\n";
const PAGE: &str = "markdown/instances/renewal/c1.md";

struct ChildGuard(Child);
impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

async fn call(p: &EscurelProcess, token: &str, name: &str, args: Value) -> Value {
    let body: Value = reqwest::Client::new()
        .post(p.mcp_url())
        .header("authorization", format!("Bearer {token}"))
        .json(&json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                       "params": { "name": name, "arguments": args } }))
        .send()
        .await
        .expect("post")
        .json()
        .await
        .expect("json");
    assert!(body.get("error").is_none(), "{name}: {body}");
    body["result"]["structuredContent"].clone()
}

/// The latest status row and its parsed body.
async fn latest(p: &EscurelProcess, token: &str) -> Option<(Value, Value)> {
    let r = call(
        p,
        token,
        "list_events",
        json!({ "label_skill": "escurel:runner-status", "newest_first": true, "limit": 1 }),
    )
    .await;
    let e = r["events"].as_array()?.first()?.clone();
    let body: Value = serde_json::from_str(e["body"].as_str()?).ok()?;
    Some((e, body))
}

async fn wait_for(
    p: &EscurelProcess,
    token: &str,
    what: &str,
    pred: impl Fn(&Value, &Value) -> bool,
) -> (Value, Value) {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if let Some((e, body)) = latest(p, token).await
            && pred(&e, &body)
        {
            return (e, body);
        }
        assert!(Instant::now() < deadline, "no status where {what}");
        tokio::time::sleep(Duration::from_millis(150)).await;
    }
}

#[tokio::test]
async fn readiness_requires_a_successful_system_event_write() {
    let gw = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(FixtureBuilder::new().tenant(TENANT).done()),
        ..Default::default()
    })
    .await;
    let listen = format!("127.0.0.1:{}", free_port());
    let ledger_dir = tempfile::tempdir().expect("tempdir");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_escurel-runner"));
    cmd.env("ESCUREL_RUNNER_LISTEN", &listen)
        .env("ESCUREL_RUNNER_GATEWAY_URL", gw.base_url())
        .env("ESCUREL_RUNNER_TENANT", TENANT)
        .env("ESCUREL_RUNNER_TOKEN", gw.mint_token(TENANT, Role::Agent))
        .env(
            "ESCUREL_RUNNER_LEDGER_PATH",
            ledger_dir.path().join("ledger.sqlite"),
        )
        .env("ESCUREL_RUNNER_STATUS_INTERVAL", "1s");
    let _runner = ChildGuard(cmd.spawn().expect("spawn runner"));
    let client = reqwest::Client::new();
    let ready_url = format!("http://{listen}/readyz");
    let health_url = format!("http://{listen}/healthz");
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if let Ok(resp) = client.get(&ready_url).send().await {
            let status = resp.status();
            let body: Value = resp.json().await.expect("readiness JSON");
            if body["reason"] == "system_event_write_refused" {
                assert_eq!(status.as_u16(), 503, "{body}");
                break;
            }
        }
        assert!(
            Instant::now() < deadline,
            "runner did not report denied system writes"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(
        client
            .get(&health_url)
            .send()
            .await
            .unwrap()
            .status()
            .as_u16(),
        200,
        "the process remains live while its control plane is unready"
    );
}

#[tokio::test]
async fn the_runner_heartbeats_and_reports_a_change_at_once() {
    let gw = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(
            FixtureBuilder::new()
                .tenant(TENANT)
                .skill("renewal", SKILL_BODY)
                .instance("renewal", "c1", INSTANCE_BODY)
                .done(),
        ),
        ..Default::default()
    })
    .await;
    let admin = gw.mint_token(TENANT, Role::Admin);

    let listen = format!("127.0.0.1:{}", free_port());
    let ledger_dir = tempfile::tempdir().expect("tempdir");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_escurel-runner"));
    cmd.env("ESCUREL_RUNNER_LISTEN", &listen)
        .env("ESCUREL_RUNNER_GATEWAY_URL", gw.base_url())
        .env("ESCUREL_RUNNER_TENANT", TENANT)
        .env("ESCUREL_RUNNER_TOKEN", &admin)
        .env("ESCUREL_RUNNER_HARNESS", "echo")
        .env("ESCUREL_RUNNER_ID", "runner-a")
        .env(
            "ESCUREL_RUNNER_LEDGER_PATH",
            ledger_dir.keep().join("ledger.sqlite"),
        )
        .env("ESCUREL_RUNNER_POLL_INTERVAL", "250ms")
        .env("ESCUREL_RUNNER_STATUS_INTERVAL", "2s")
        .env("ESCUREL_ECHO_SLEEP_MS", "3000");
    let _runner = ChildGuard(cmd.spawn().expect("spawn runner"));

    // Boot: the first status says who and what, idle.
    let (e, body) = wait_for(&gw, &admin, "the runner has booted", |_, _| true).await;
    let ready_by = Instant::now() + Duration::from_secs(5);
    loop {
        let readiness: Value = reqwest::get(format!("http://{listen}/readyz"))
            .await
            .expect("GET runner readiness")
            .json()
            .await
            .expect("readiness JSON");
        if readiness["ready"] == true {
            assert_eq!(readiness["reason"], "ready");
            break;
        }
        assert!(
            Instant::now() < ready_by,
            "runner stayed unready: {readiness}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(e["kind"], "system", "{e}");
    assert_eq!(
        e["status"], "inbox",
        "unassigned bookkeeping, hidden from the inbox: {e}"
    );
    assert_eq!(body["runner_id"], "runner-a", "{body}");
    assert_eq!(body["harness"], "echo", "{body}");
    assert_eq!(body["tenant"], TENANT, "{body}");
    assert!(body["version"].is_string(), "{body}");
    assert_eq!(body["live_runs"], json!([]), "{body}");
    assert_eq!(body["paused_tenants"], json!([]), "{body}");
    assert_eq!(body["runs"]["dead_letter"], 0, "{body}");
    assert!(body["uptime_s"].is_number(), "{body}");
    let inbox = call(&gw, &admin, "list_inbox", json!({})).await;
    assert!(inbox["events"].as_array().unwrap().is_empty(), "{inbox}");

    // A run starts: a `changed` status names it as live, before any
    // scheduled heartbeat could.
    let r = call(
        &gw,
        &admin,
        "capture_event",
        json!({ "source": "manual", "mime": "text/plain", "label_skill": "renewal",
                "instance_page_id": PAGE, "title": "renew", "body": "please renew" }),
    )
    .await;
    let event_id = r["event_id"].as_str().unwrap().to_owned();
    let (e, body) = wait_for(&gw, &admin, "a run is live", |_, b| {
        b["live_runs"].as_array().is_some_and(|a| a.len() == 1)
    })
    .await;
    assert_eq!(e["title"], "changed", "{e}");
    assert_eq!(body["live_runs"][0]["event_id"], event_id, "{body}");
    assert!(body["live_runs"][0]["run_id"].is_string(), "{body}");
    assert!(
        body["last_poll_age_ms"]
            .as_u64()
            .is_some_and(|ms| ms < 5000),
        "{body}"
    );

    // …and ends: idle again, with the run counted.
    let (_, body) = wait_for(&gw, &admin, "the run has ended", |_, b| {
        b["live_runs"].as_array().is_some_and(Vec::is_empty) && b["runs"]["processed"] == 1
    })
    .await;
    assert_eq!(body["runs"]["pending"], 0, "{body}");

    // A change of policy is reported at once too.
    call(
        &gw,
        &admin,
        "capture_event",
        json!({ "source": "workbench", "mime": "application/json",
                "label_skill": "escurel:run-control", "title": "pause",
                "body": json!({ "action": "pause" }).to_string() }),
    )
    .await;
    let (e, _) = wait_for(&gw, &admin, "the tenant is paused", |_, b| {
        b["paused_tenants"] == json!([TENANT])
    })
    .await;
    assert_eq!(e["title"], "changed", "{e}");

    // The record keeps being refreshed while nothing changes, and stays ONE row.
    let at_of = |e: &Value| e["at"].as_str().unwrap_or_default().to_owned();
    let (e0, _) = latest(&gw, &admin).await.expect("record");
    let before = at_of(&e0);
    tokio::time::sleep(Duration::from_millis(4500)).await;
    let (e1, body) = latest(&gw, &admin).await.expect("record");
    assert_ne!(
        at_of(&e1),
        before,
        "the heartbeat refreshed the record: {e1}"
    );
    assert_eq!(e1["title"], "heartbeat", "{e1}");
    assert_eq!(body["state"], "heartbeat", "{body}");
    assert_eq!(body["status_interval_s"], 2, "{body}");
    assert!(body["started_at"].is_string(), "{body}");
    assert!(body["last_change_at"].is_string(), "{body}");
    let rows = call(
        &gw,
        &admin,
        "list_events",
        json!({ "label_skill": "escurel:runner-status", "limit": 1000, "include_system": true }),
    )
    .await;
    let rows = rows["events"].as_array().unwrap();
    assert_eq!(rows.len(), 1, "one record, however many reports: {rows:?}");
    assert_eq!(rows[0]["event_id"], "runner-status:runner-a");
}

/// A thin reverse proxy in front of a REAL gateway that answers `-32601` to
/// `report_runner_status` and forwards everything else: what a gateway built
/// before the tool does with it. Returns its address and the count of
/// forwarded `capture_event` calls on the status label.
async fn gateway_without_the_tool(
    upstream: String,
) -> (String, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
    use axum::body::Bytes;
    use axum::extract::State;
    use axum::http::{HeaderMap, Method, StatusCode, Uri};
    use axum::response::{IntoResponse, Response};
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[derive(Clone)]
    struct S {
        upstream: String,
        http: reqwest::Client,
        status_captures: std::sync::Arc<AtomicUsize>,
    }
    async fn forward(
        State(s): State<S>,
        method: Method,
        uri: Uri,
        headers: HeaderMap,
        body: Bytes,
    ) -> Response {
        if let Ok(v) = serde_json::from_slice::<Value>(&body) {
            let tool = v["params"]["name"].as_str().unwrap_or_default();
            if tool == "report_runner_status" {
                return axum::Json(json!({ "jsonrpc": "2.0", "id": v["id"],
                    "error": { "code": -32601, "message": "unknown tool `report_runner_status`" } }))
                .into_response();
            }
            if tool == "capture_event"
                && v["params"]["arguments"]["label_skill"] == "escurel:runner-status"
            {
                s.status_captures.fetch_add(1, Ordering::SeqCst);
            }
        }
        let mut req = s.http.request(
            reqwest::Method::from_bytes(method.as_str().as_bytes()).unwrap(),
            format!(
                "{}{}",
                s.upstream,
                uri.path_and_query().map_or("", |p| p.as_str())
            ),
        );
        for (k, v) in &headers {
            if k != "host" && k != "content-length" {
                req = req.header(k.as_str(), v.as_bytes());
            }
        }
        match req.body(body.to_vec()).send().await {
            Ok(r) => {
                let status = StatusCode::from_u16(r.status().as_u16()).unwrap();
                let ct = r.headers().get("content-type").cloned();
                let bytes = r.bytes().await.unwrap_or_default();
                let mut out = (status, bytes).into_response();
                if let Some(ct) = ct {
                    out.headers_mut().insert("content-type", ct);
                }
                out
            }
            Err(_) => StatusCode::BAD_GATEWAY.into_response(),
        }
    }
    let count = std::sync::Arc::new(AtomicUsize::new(0));
    let app = axum::Router::new().fallback(forward).with_state(S {
        upstream,
        http: reqwest::Client::new(),
        status_captures: count.clone(),
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (format!("http://{addr}"), count)
}

#[tokio::test]
async fn a_runner_falls_back_to_heartbeat_events_on_a_gateway_without_the_tool() {
    let gw = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(FixtureBuilder::new().tenant(TENANT).done()),
        ..Default::default()
    })
    .await;
    let admin = gw.mint_token(TENANT, Role::Admin);
    let (old_gateway, status_captures) = gateway_without_the_tool(gw.base_url().to_owned()).await;

    let listen = format!("127.0.0.1:{}", free_port());
    let ledger_dir = tempfile::tempdir().expect("tempdir");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_escurel-runner"));
    cmd.env("ESCUREL_RUNNER_LISTEN", &listen)
        .env("ESCUREL_RUNNER_GATEWAY_URL", &old_gateway)
        .env("ESCUREL_RUNNER_TENANT", TENANT)
        .env("ESCUREL_RUNNER_TOKEN", &admin)
        .env("ESCUREL_RUNNER_ID", "runner-old")
        .env(
            "ESCUREL_RUNNER_LEDGER_PATH",
            ledger_dir.path().join("ledger.sqlite"),
        )
        .env("ESCUREL_RUNNER_STATUS_INTERVAL", "1s");
    let _runner = ChildGuard(cmd.spawn().expect("spawn runner"));

    // Heartbeat EVENTS arrive (several rows, none of them the record), and the
    // runner is ready: the fallback write is the readiness write.
    let deadline = Instant::now() + Duration::from_secs(25);
    loop {
        let rows = call(
            &gw,
            &admin,
            "list_events",
            json!({ "label_skill": "escurel:runner-status", "limit": 1000, "include_system": true }),
        )
        .await;
        let rows = rows["events"].as_array().unwrap().clone();
        if rows.len() >= 3 {
            assert!(
                rows.iter()
                    .all(|e| e["event_id"] != "runner-status:runner-old"),
                "{rows:?}"
            );
            assert!(rows.iter().any(|e| e["title"] == "heartbeat"), "{rows:?}");
            break;
        }
        assert!(
            Instant::now() < deadline,
            "no fallback heartbeats: {rows:?}"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    let ready: Value = reqwest::get(format!("http://{listen}/readyz"))
        .await
        .expect("readyz")
        .json()
        .await
        .expect("json");
    assert_eq!(ready["ready"], true, "{ready}");
    assert!(status_captures.load(std::sync::atomic::Ordering::SeqCst) >= 3);
}
