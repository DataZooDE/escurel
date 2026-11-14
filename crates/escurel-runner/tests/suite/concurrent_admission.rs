//! Concurrent admission (#828): one poll claims its inbox events with up to
//! `ESCUREL_RUNNER_ADMIT_CONCURRENCY` `claim_event` calls in flight, instead
//! of one 2-second-bounded call after another.
//!
//! A gateway stand-in sits in front of the real gateway and holds every
//! `claim_event` for a fixed second, so admission time is the measure: eight
//! inbox events take about eight seconds one at a time and about one at the
//! default concurrency. The bounds are generous both ways (serial ≥ 7 s,
//! concurrent < 4 s) so neither side is flaky. Real gateway, real runner,
//! real echo harness; the proxy only delays.

use std::time::{Duration, Instant};

use escurel_test_support::{AuthMode, EscurelProcess, FixtureBuilder, Opts, Role};
use serde_json::json;

use crate::support::{RunnerProcess, RunningRunner, call};

const TENANT: &str = "acme";
const NOTE: &str =
    "---\nkind: skill\nid: note\nautonomy: auto\n---\n# note\n\nFold the event in.\n";
const EVENTS: usize = 8;
const CLAIM_DELAY: Duration = Duration::from_secs(1);

async fn gateway() -> EscurelProcess {
    EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(
            FixtureBuilder::new()
                .tenant(TENANT)
                .skill("note", NOTE)
                .instance(
                    "note",
                    "c1",
                    "---\nkind: instance\nid: c1\nskill: note\n---\n# C1\n\nBASELINE.\n",
                )
                .done(),
        ),
        ..Default::default()
    })
    .await
}

/// Forward everything to `upstream`; hold a `claim_event` for [`CLAIM_DELAY`]
/// first. Returns the stand-in's base URL.
async fn delaying_proxy(upstream: String) -> String {
    use axum::body::Bytes;
    use axum::extract::State;
    use axum::http::{HeaderMap, Method, StatusCode, Uri};
    use axum::response::IntoResponse;

    async fn forward(
        State(upstream): State<String>,
        method: Method,
        uri: Uri,
        headers: HeaderMap,
        body: Bytes,
    ) -> axum::response::Response {
        if body.windows(13).any(|w| w == b"\"claim_event\"") {
            tokio::time::sleep(CLAIM_DELAY).await;
        }
        let mut req = reqwest::Client::new().request(
            reqwest::Method::from_bytes(method.as_str().as_bytes()).unwrap(),
            format!(
                "{upstream}{}",
                uri.path_and_query().map_or("/", |p| p.as_str())
            ),
        );
        for name in ["authorization", "content-type", "accept"] {
            if let Some(v) = headers.get(name).and_then(|v| v.to_str().ok()) {
                req = req.header(name, v);
            }
        }
        match req.body(body.to_vec()).send().await {
            Ok(resp) => {
                let status = StatusCode::from_u16(resp.status().as_u16()).unwrap();
                let ctype = resp
                    .headers()
                    .get("content-type")
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("application/json")
                    .to_owned();
                let bytes = resp.bytes().await.unwrap_or_default();
                (status, [("content-type", ctype)], bytes.to_vec()).into_response()
            }
            Err(e) => (StatusCode::BAD_GATEWAY, e.to_string()).into_response(),
        }
    }

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = axum::Router::new()
        .fallback(forward)
        .with_state(upstream.trim_end_matches('/').to_owned());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{addr}")
}

/// How long `concurrency` takes to admit [`EVENTS`] inbox events behind the
/// delaying stand-in (until every one has a ledger run).
async fn admission_time(concurrency: &str) -> Duration {
    let gw = gateway().await;
    let admin = gw.mint_token(TENANT, Role::Admin);
    let mut events = Vec::new();
    for n in 0..EVENTS {
        let r = call(
            &gw,
            &admin,
            "capture_event",
            json!({ "source": "test", "mime": "text/plain", "label_skill": "note",
                    "instance_page_id": "markdown/instances/note/c1.md",
                    "title": format!("work {n}"), "body": "work" }),
        )
        .await;
        events.push(r["event_id"].as_str().unwrap().to_owned());
    }
    let proxy = delaying_proxy(gw.base_url().to_owned()).await;
    let mut runner: RunningRunner = RunnerProcess::echo(&gw, TENANT)
        .env("ESCUREL_RUNNER_GATEWAY_URL", proxy)
        .env("ESCUREL_RUNNER_ADMIT_CONCURRENCY", concurrency)
        .spawn();
    runner.wait_healthy().await;
    let started = Instant::now();
    let deadline = started + Duration::from_secs(40);
    loop {
        let mut admitted = 0;
        for e in &events {
            if runner.debug_run(e).await.is_some() {
                admitted += 1;
            }
        }
        if admitted == EVENTS {
            return started.elapsed();
        }
        assert!(
            Instant::now() < deadline,
            "only {admitted}/{EVENTS} admitted at concurrency {concurrency}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test]
async fn a_poll_claims_its_inbox_concurrently_and_serially_only_when_asked() {
    let (serial, concurrent) = tokio::join!(admission_time("1"), admission_time("8"));
    assert!(
        serial >= Duration::from_secs(7),
        "the stand-in holds each claim a second, so one at a time takes ~8 s: {serial:?}"
    );
    assert!(
        concurrent < Duration::from_secs(4),
        "eight claims in flight together take ~1 s, not {concurrent:?} (serial: {serial:?})"
    );
}
