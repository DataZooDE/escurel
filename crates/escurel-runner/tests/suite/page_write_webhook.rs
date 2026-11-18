//! A gateway's `page_write` notification is not work (heron-escurel,
//! 2026-11-18).
//!
//! With `ESCUREL_WEBHOOK_URL` pointed at the runner's `/trigger`, the gateway
//! POSTs a `page_write` notification after every confirmed instance write
//! (`emit_page_event`: "notification, not work"). It carries no `event_id`.
//! The runner parsed it as an [`escurel_types::Event`] anyway, failed to claim
//! the empty id, held it as a lone runner and dispatched a run for the page's
//! skill: on heron the runner's own `runtime` page write produced a run for
//! a skill `runtime` that does not exist, dead-lettered.
//!
//! Real gateway, real webhook, real runner binary: an instance write must
//! reach the runner (asserted on the gateway's delivery log, so the test
//! cannot pass by the notification never arriving) and create no run, while
//! a captured event on the same webhook still runs (the positive control).

use std::time::{Duration, Instant};

use escurel_test_support::{
    AuthMode, ConfigOverrides, EscurelProcess, FixtureBuilder, Opts, Role, free_port,
};
use serde_json::{Value, json};

use crate::support::{RunnerProcess, call};

const TENANT: &str = "acme";

const NOTE: &str =
    "---\nkind: skill\nid: note\nautonomy: auto\n---\n# note\n\nFold the event in.\n";

const PAGE: &str = "markdown/instances/note/c1.md";

async fn ledger(listen: &str) -> Value {
    reqwest::get(format!("http://{listen}/debug/ledger"))
        .await
        .expect("GET /debug/ledger")
        .json()
        .await
        .expect("ledger json")
}

async fn eventually<F, Fut>(what: &str, mut probe: F) -> Value
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Option<Value>>,
{
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Some(v) = probe().await {
            return v;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

#[tokio::test]
async fn a_page_write_notification_reaches_the_runner_and_starts_no_run() {
    let listen = format!("127.0.0.1:{}", free_port());
    let gw = EscurelProcess::spawn(Opts {
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
        config_overrides: ConfigOverrides {
            webhook_url: Some(format!("http://{listen}/trigger")),
            ..Default::default()
        },
    })
    .await;
    let admin = gw.mint_token(TENANT, Role::Admin);
    let _runner = RunnerProcess::echo(&gw, TENANT)
        .static_bearer(gw.mint_token_with_sub(TENANT, Role::Admin, "runner:hq"))
        .env("ESCUREL_RUNNER_LISTEN", listen.clone())
        // Poll rarely: the webhook, not the poller, is the path under test.
        .env("ESCUREL_RUNNER_POLL_INTERVAL", "60s")
        .spawn();
    eventually("the runner's /healthz", || async {
        reqwest::get(format!("http://{listen}/healthz"))
            .await
            .ok()
            .filter(|r| r.status().is_success())
            .map(|_| Value::Null)
    })
    .await;

    // An ordinary instance write, as a person approving a draft makes it.
    let page = call(
        &gw,
        &admin,
        "expand",
        json!({ "page_id": PAGE, "raw": true }),
    )
    .await;
    let sha = page["content_sha256"]
        .as_str()
        .expect("content_sha256")
        .to_owned();
    call(
        &gw,
        &admin,
        "update_page",
        json!({
            "page_id": PAGE,
            "content": "---\nkind: instance\nid: c1\nskill: note\n---\n# C1\n\nEDITED.\n",
            "base_sha256": sha,
        }),
    )
    .await;

    // The notification reached the runner and was acknowledged...
    eventually("the page_write delivery", || async {
        let d = call(&gw, &admin, "admin_webhook_deliveries", json!({})).await;
        d["deliveries"]
            .as_array()?
            .iter()
            .find(|r| r["event_id"] == "" && r["http_status"] == 202)
            .cloned()
    })
    .await;

    // ...and a captured event over the same webhook still runs.
    call(
        &gw,
        &admin,
        "capture_event",
        json!({ "source": "test", "mime": "text/plain", "label_skill": "note",
                "instance_page_id": PAGE, "title": "t", "body": "work" }),
    )
    .await;
    let settled = eventually("the control run to finish", || async {
        let l = ledger(&listen).await;
        (l["succeeded"].as_u64() >= Some(1)).then_some(l)
    })
    .await;

    assert_eq!(
        settled["total"], 1,
        "only the captured event makes a run; the page_write notification must not: {settled}"
    );
    assert_eq!(settled["dead_letter"], 0, "{settled}");
    assert_eq!(settled["failed"], 0, "{settled}");
}
