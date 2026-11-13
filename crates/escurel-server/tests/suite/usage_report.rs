//! `usage_report` (#821): usage and cost rolled up from `run-finished`.
//!
//! Real gateway, real test issuer. The `run-finished` rows are filed the way
//! the runner files them — `capture_event` under `escurel:run`, `kind:
//! system`, with an operator ADMIN bearer that is not bound to a run (the
//! only caller the `escurel:` namespace admits) — with known usage across two
//! skills, two models and two days, plus one run outside the window and a
//! `run-started` that must not count. The report must return the exact sums
//! per grouping, a total row, honour the window, and refuse a non-admin.

use escurel_test_support::{AuthMode, EscurelProcess, FixtureBuilder, Opts, Role};
use serde_json::{Value, json};

const TENANT: &str = "acme";
const PAGE_RENEWAL: &str = "markdown/instances/renewal/c1.md";
const PAGE_HYGIENE: &str = "markdown/instances/hygiene/c2.md";
const SINCE: &str = "2026-10-01T00:00:00Z";
const UNTIL: &str = "2026-10-03T00:00:00Z";

async fn gateway() -> EscurelProcess {
    EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(
            FixtureBuilder::new()
                .tenant(TENANT)
                .skill(
                    "renewal",
                    "---\nkind: skill\nid: renewal\n---\n# renewal\n\nFold renewals.\n",
                )
                .skill(
                    "hygiene",
                    "---\nkind: skill\nid: hygiene\n---\n# hygiene\n\nKeep it tidy.\n",
                )
                .instance(
                    "renewal",
                    "c1",
                    "---\nkind: instance\nid: c1\nskill: renewal\n---\n# C1\n",
                )
                .instance(
                    "hygiene",
                    "c2",
                    "---\nkind: instance\nid: c2\nskill: hygiene\n---\n# C2\n",
                )
                .done(),
        ),
        ..Default::default()
    })
    .await
}

/// The raw JSON-RPC response, so a refusal can be asserted on.
async fn rpc(p: &EscurelProcess, token: &str, name: &str, args: Value) -> Value {
    reqwest::Client::new()
        .post(p.mcp_url())
        .header("authorization", format!("Bearer {token}"))
        .json(&json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                       "params": { "name": name, "arguments": args } }))
        .send()
        .await
        .expect("post")
        .json()
        .await
        .expect("json")
}

async fn call(p: &EscurelProcess, name: &str, args: Value) -> Value {
    let token = p.mint_token(TENANT, Role::Admin);
    let body = rpc(p, &token, name, args).await;
    assert!(body.get("error").is_none(), "{name}: {body}");
    body["result"]["structuredContent"].clone()
}

struct Run {
    id: &'static str,
    skill: &'static str,
    page: &'static str,
    model: &'static str,
    at: &'static str,
    status: &'static str,
    usage: Option<(u64, u64, f64)>,
}

/// One `run-finished` (and its `run-started`, which the report must skip),
/// shaped like `RunEventCtx::emit_finished` writes it.
async fn file_run(p: &EscurelProcess, r: &Run) {
    let usage = r.usage.map(|(i, o, c)| {
        json!({ "input_tokens": i, "output_tokens": o, "cost_usd": c, "model": r.model })
    });
    let runner = json!({ "runner": {
        "run_id": r.id, "root_event_id": format!("ev-{}", r.id), "event_id": format!("ev-{}", r.id),
        "skill": r.skill, "harness": "echo", "model": r.model, "target_page_id": r.page,
    }});
    for (suffix, title, body) in [
        ("started", "run-started", json!({})),
        (
            "finished",
            "run-finished",
            json!({ "status": r.status, "attempts": 1, "usage": usage }),
        ),
    ] {
        call(
            p,
            "capture_event",
            json!({
                "event_id": format!("run:{}:{suffix}", r.id),
                "at": r.at,
                "source": "escurel-runner",
                "mime": "application/json",
                "label_skill": "escurel:run",
                "instance_page_id": r.page,
                "title": title,
                "body": body.to_string(),
                "provenance": runner,
                "kind": "system",
            }),
        )
        .await;
    }
}

async fn seed(p: &EscurelProcess) {
    let runs = [
        Run {
            id: "r1",
            skill: "renewal",
            page: PAGE_RENEWAL,
            model: "m-a",
            at: "2026-10-01T09:00:00Z",
            status: "processed",
            usage: Some((100, 10, 0.25)),
        },
        Run {
            id: "r2",
            skill: "renewal",
            page: PAGE_RENEWAL,
            model: "m-b",
            at: "2026-10-01T18:00:00Z",
            status: "failed",
            usage: Some((200, 20, 0.5)),
        },
        Run {
            id: "r3",
            skill: "hygiene",
            page: PAGE_HYGIENE,
            model: "m-a",
            at: "2026-10-02T07:30:00Z",
            status: "processed",
            usage: Some((300, 30, 1.0)),
        },
        // No usage reported: still a run, and its model comes from the
        // run's own provenance. Its cost is unknown, not $0: it counts as
        // an unpriced run (crew review PR-1, F5).
        Run {
            id: "r4",
            skill: "hygiene",
            page: PAGE_HYGIENE,
            model: "m-a",
            at: "2026-10-02T23:59:59Z",
            status: "processed",
            usage: None,
        },
        // Outside the window.
        Run {
            id: "r5",
            skill: "renewal",
            page: PAGE_RENEWAL,
            model: "m-a",
            at: "2026-09-15T12:00:00Z",
            status: "processed",
            usage: Some((1000, 1000, 8.0)),
        },
    ];
    for r in &runs {
        file_run(p, r).await;
    }
}

async fn report(p: &EscurelProcess, args: Value) -> Value {
    call(p, "usage_report", args).await
}

#[tokio::test]
async fn usage_report_sums_runs_per_grouping_within_the_window() {
    let p = gateway().await;
    seed(&p).await;
    let window = |group_by: Value| json!({ "since": SINCE, "until": UNTIL, "group_by": group_by });

    let by_skill = report(&p, window(json!(["skill"]))).await;
    assert_eq!(
        by_skill["rows"],
        json!([
            { "group": { "skill": "hygiene" }, "runs": 2, "by_status": { "processed": 2 },
              "input_tokens": 300, "output_tokens": 30, "cost_usd": 1.0,
              "priced_runs": 1, "unpriced_runs": 1 },
            { "group": { "skill": "renewal" }, "runs": 2, "by_status": { "failed": 1, "processed": 1 },
              "input_tokens": 300, "output_tokens": 30, "cost_usd": 0.75,
              "priced_runs": 2, "unpriced_runs": 0 },
        ]),
        "{by_skill}"
    );
    assert_eq!(
        by_skill["total"],
        json!({ "runs": 4, "by_status": { "failed": 1, "processed": 3 },
                "input_tokens": 600, "output_tokens": 60, "cost_usd": 1.75,
                "priced_runs": 3, "unpriced_runs": 1 }),
        "{by_skill}"
    );

    let by_skill_model = report(&p, window(json!(["skill", "model"]))).await;
    let groups: Vec<(Value, u64, f64)> = by_skill_model["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| {
            (
                r["group"].clone(),
                r["runs"].as_u64().unwrap(),
                r["cost_usd"].as_f64().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        groups,
        vec![
            (json!({ "skill": "hygiene", "model": "m-a" }), 2, 1.0),
            (json!({ "skill": "renewal", "model": "m-a" }), 1, 0.25),
            (json!({ "skill": "renewal", "model": "m-b" }), 1, 0.5),
        ],
        "{by_skill_model}"
    );

    let by_day = report(&p, window(json!(["day"]))).await;
    let days: Vec<(Value, u64, u64)> = by_day["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| {
            (
                r["group"]["day"].clone(),
                r["runs"].as_u64().unwrap(),
                r["input_tokens"].as_u64().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        days,
        vec![(json!("2026-10-01"), 2, 300), (json!("2026-10-02"), 2, 300)],
        "{by_day}"
    );

    let by_agent_page = report(&p, window(json!(["agent", "page"]))).await;
    let groups: Vec<Value> = by_agent_page["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["group"].clone())
        .collect();
    assert_eq!(
        groups,
        vec![
            json!({ "agent": "agent:hygiene", "page": PAGE_HYGIENE }),
            json!({ "agent": "agent:renewal", "page": PAGE_RENEWAL }),
        ],
        "{by_agent_page}"
    );

    // `skill` narrows before grouping.
    let hygiene = report(
        &p,
        json!({ "since": SINCE, "until": UNTIL, "group_by": ["model"], "skill": "hygiene" }),
    )
    .await;
    assert_eq!(hygiene["total"]["runs"], json!(2), "{hygiene}");
    assert_eq!(hygiene["rows"][0]["group"], json!({ "model": "m-a" }));

    // No window: the September run counts too.
    let all = report(&p, json!({ "group_by": ["skill"] })).await;
    assert_eq!(all["total"]["runs"], json!(5), "{all}");
    assert_eq!(all["total"]["input_tokens"], json!(1600), "{all}");
    assert_eq!(all["total"]["cost_usd"], json!(9.75), "{all}");

    // `until` is exclusive, `since` inclusive.
    let one_day = report(
        &p,
        json!({ "since": "2026-10-01T09:00:00Z", "until": "2026-10-02T23:59:59Z",
                "group_by": ["day"] }),
    )
    .await;
    assert_eq!(one_day["total"]["runs"], json!(3), "{one_day}");
}

#[tokio::test]
async fn list_events_honours_since_and_until() {
    let p = gateway().await;
    seed(&p).await;
    let page = call(
        &p,
        "list_events",
        json!({ "label_skill": "escurel:run", "since": SINCE, "until": UNTIL }),
    )
    .await;
    let mut ids: Vec<String> = page["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["event_id"].as_str().unwrap().to_owned())
        .collect();
    ids.sort();
    let mut want: Vec<String> = ["r1", "r2", "r3", "r4"]
        .iter()
        .flat_map(|r| [format!("run:{r}:finished"), format!("run:{r}:started")])
        .collect();
    want.sort();
    assert_eq!(ids, want, "{page}");

    let bad = rpc(
        &p,
        &p.mint_token(TENANT, Role::Admin),
        "list_events",
        json!({ "label_skill": "escurel:run", "since": "yesterday" }),
    )
    .await;
    assert_eq!(bad["error"]["code"], json!(-32602), "{bad}");
}

#[tokio::test]
async fn usage_report_is_admin_only_and_checks_its_arguments() {
    let p = gateway().await;
    let agent = p.mint_token(TENANT, Role::Agent);
    let refused = rpc(&p, &agent, "usage_report", json!({ "group_by": ["skill"] })).await;
    assert_eq!(refused["error"]["code"], json!(-32001), "{refused}");

    let admin = p.mint_token(TENANT, Role::Admin);
    for args in [
        json!({ "group_by": ["colour"] }),
        json!({ "group_by": [] }),
        json!({ "group_by": ["skill"], "since": "not-a-time" }),
    ] {
        let bad = rpc(&p, &admin, "usage_report", args.clone()).await;
        assert_eq!(bad["error"]["code"], json!(-32602), "{args}: {bad}");
    }
}
