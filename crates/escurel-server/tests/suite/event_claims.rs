//! Distributed runtimes, gateway side (#818): claim leases, `requires:`
//! validation and the runtime directory.
//!
//! Real gateway over HTTP, real test issuer. `claim_event` is the
//! compare-and-set that decides which runner starts an event: N concurrent
//! claims must yield exactly one winner, a lapsed lease is claimable again,
//! `assign_event` releases it, and only a runner's own admin identity may
//! claim. `list_runtimes` joins `runtime` pages with the runners' status
//! records, deriving online / stale / stopped.

use std::time::Duration;

use escurel_test_support::{AuthMode, EscurelProcess, FixtureBuilder, Opts, Role};
use serde_json::{Value, json};

const TENANT: &str = "acme";
const NOTE: &str = "---\nkind: skill\nid: note\ndescription: n\n\
    acl:\n  create: [public]\n  update: [public]\n---\n# note\n";

async fn gateway() -> EscurelProcess {
    EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(
            FixtureBuilder::new()
                .tenant(TENANT)
                .skill("note", NOTE)
                .instance(
                    "note",
                    "n1",
                    "---\nkind: instance\nskill: note\nid: n1\n---\n# n1\n",
                )
                .done(),
        ),
        ..Default::default()
    })
    .await
}

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

async fn call(p: &EscurelProcess, token: &str, name: &str, args: Value) -> Value {
    let body = rpc(p, token, name, args).await;
    assert!(body.get("error").is_none(), "{name}: {body}");
    body["result"]["structuredContent"].clone()
}

fn admin(p: &EscurelProcess) -> String {
    p.mint_token(TENANT, Role::Admin)
}

async fn capture(p: &EscurelProcess, label: &str) -> String {
    let r = call(
        p,
        &admin(p),
        "capture_event",
        json!({ "source": "test", "mime": "text/plain", "label_skill": label,
                "title": "t", "body": "b" }),
    )
    .await;
    r["event_id"].as_str().unwrap().to_owned()
}

/// Many runners race for one event: exactly one wins, the rest see it.
#[tokio::test]
async fn concurrent_claims_have_exactly_one_winner() {
    let p = gateway().await;
    let event = capture(&p, "note").await;
    let token = admin(&p);
    let mut handles = Vec::new();
    for n in 0..16 {
        let (url, token, event) = (p.mcp_url(), token.clone(), event.clone());
        handles.push(tokio::spawn(async move {
            let body: Value = reqwest::Client::new()
                .post(url)
                .header("authorization", format!("Bearer {token}"))
                .json(&json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                    "params": { "name": "claim_event",
                                "arguments": { "event_id": event, "holder_id": format!("r{n}") } } }))
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
            body["result"]["structuredContent"].clone()
        }));
    }
    let mut results = Vec::new();
    for h in handles {
        results.push(h.await.unwrap());
    }
    let winners: Vec<&Value> = results.iter().filter(|r| r["claimed"] == true).collect();
    assert_eq!(winners.len(), 1, "{results:?}");
    let holder = winners[0]["holder"].as_str().unwrap().to_owned();
    assert!(
        holder.contains("#r"),
        "the holder names the runner: {holder}"
    );
    for r in &results {
        assert_eq!(
            r["holder"],
            json!(holder),
            "every loser sees the winner: {r}"
        );
        assert!(
            r["expires_at"].as_str().is_some_and(|t| t.ends_with('Z')),
            "{r}"
        );
    }

    // The by-id lookup says who holds it.
    let looked = call(&p, &token, "list_events", json!({ "event_id": event })).await;
    assert_eq!(looked["claim"]["holder"], json!(holder), "{looked}");
}

/// A lapsed lease is claimable; renew never takes over; assign releases.
#[tokio::test]
async fn a_lease_lapses_renews_and_is_released_by_assign() {
    let p = gateway().await;
    let event = capture(&p, "note").await;
    let token = admin(&p);
    let claim = |runner: &str, lease: u64| json!({ "event_id": event, "holder_id": runner, "lease_secs": lease });
    let a = call(&p, &token, "claim_event", claim("a", 1)).await;
    assert_eq!(a["claimed"], true, "{a}");
    let b = call(&p, &token, "claim_event", claim("b", 30)).await;
    assert_eq!(b["claimed"], false, "a live lease is not taken: {b}");
    let renewed = call(&p, &token, "renew_claim", claim("a", 1)).await;
    assert_eq!(renewed["renewed"], true, "{renewed}");

    tokio::time::sleep(Duration::from_millis(1300)).await;
    let renew_b = call(&p, &token, "renew_claim", claim("b", 30)).await;
    assert_eq!(
        renew_b["renewed"], false,
        "renew never takes over: {renew_b}"
    );
    let b = call(&p, &token, "claim_event", claim("b", 30)).await;
    assert_eq!(b["claimed"], true, "a lapsed lease is claimable: {b}");
    let a = call(&p, &token, "renew_claim", claim("a", 30)).await;
    assert_eq!(a["renewed"], false, "the old holder lost it: {a}");
    assert!(a["holder"].as_str().unwrap().ends_with("#b"), "{a}");

    call(
        &p,
        &token,
        "assign_event",
        json!({ "event_id": event, "instance_page_id": "markdown/instances/note/n1.md" }),
    )
    .await;
    let looked = call(&p, &token, "list_events", json!({ "event_id": event })).await;
    assert!(
        looked.get("claim").is_none(),
        "assign released it: {looked}"
    );
    let after = call(&p, &token, "claim_event", claim("a", 30)).await;
    assert_eq!(after["claimed"], false, "{after}");
    assert_eq!(after["processed"], true, "{after}");
}

/// Only the runner's own admin identity claims: a person, and a run token
/// (even an admin-role one), are refused; a missing event is not found.
#[tokio::test]
async fn only_a_runner_identity_may_claim() {
    let p = gateway().await;
    let event = capture(&p, "note").await;
    let member = p.mint_token_with_sub(TENANT, Role::Agent, "member:alice");
    let run = p.mint_token_for_run(TENANT, Role::Admin, "agent:note", "run-1", "root-1");
    for (who, token) in [("member", &member), ("run token", &run)] {
        for tool in ["claim_event", "renew_claim"] {
            let body = rpc(&p, token, tool, json!({ "event_id": event })).await;
            assert!(body.get("error").is_some(), "{who} {tool}: {body}");
        }
    }
    let body = rpc(&p, &member, "claim_event", json!({ "event_id": event })).await;
    assert_eq!(body["error"]["code"], -32001, "admin-scope refusal: {body}");
    let body = rpc(&p, &run, "claim_event", json!({ "event_id": event })).await;
    assert_eq!(body["error"]["data"]["code"], "forbidden", "{body}");

    let missing = rpc(&p, &admin(&p), "claim_event", json!({ "event_id": "nope" })).await;
    assert_eq!(
        missing["error"]["data"]["code"], "event_not_found",
        "{missing}"
    );
    let bad = rpc(
        &p,
        &admin(&p),
        "claim_event",
        json!({ "event_id": event, "lease_secs": 0 }),
    )
    .await;
    assert_eq!(bad["error"]["code"], -32602, "{bad}");
}

/// `requires:` is validated on skill pages and refused at capture when malformed.
#[tokio::test]
async fn requires_is_validated_on_skills_and_events() {
    let p = gateway().await;
    let token = admin(&p);
    let skill = |requires: &str| {
        format!("---\nkind: skill\nid: field\ndescription: d\nrequires: {requires}\n---\n# field\n")
    };
    let ok = call(
        &p,
        &token,
        "validate",
        json!({ "content": skill("{harness: codex, labels: [\"site:muc\"]}") }),
    )
    .await;
    assert_eq!(ok["ok"], true, "{ok}");
    let bad = call(&p, &token, "validate", json!({ "content": skill("codex") })).await;
    assert_eq!(bad["ok"], false, "{bad}");
    assert!(
        bad["issues"]
            .as_array()
            .unwrap()
            .iter()
            .any(|i| i["code"] == "requires_invalid" && i["location"] == "frontmatter.requires"),
        "{bad}"
    );

    // The skill catalogue projects it, so a runner can route without expanding.
    call(
        &p,
        &token,
        "update_page",
        json!({ "page_id": "markdown/skills/field.md",
                "content": skill("{labels: [\"has:sap-gui\"]}") }),
    )
    .await;
    let skills = call(&p, &token, "list_skills", json!({})).await;
    let field = skills["skills"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["id"] == "field")
        .cloned()
        .unwrap();
    assert_eq!(
        field["requires"],
        json!({ "labels": ["has:sap-gui"] }),
        "{field}"
    );

    let refused = rpc(
        &p,
        &token,
        "capture_event",
        json!({ "source": "t", "mime": "text/plain", "label_skill": "note", "title": "t",
                "body": "b", "provenance": { "requires": { "labels": "has:x" } } }),
    )
    .await;
    assert_eq!(
        refused["error"]["data"]["code"], "requires_invalid",
        "{refused}"
    );
    call(
        &p,
        &token,
        "capture_event",
        json!({ "source": "t", "mime": "text/plain", "label_skill": "note", "title": "t",
                "body": "b", "provenance": { "requires": { "harness": "codex" } } }),
    )
    .await;
}

/// The `runtime` system skill is admin-written; `list_runtimes` derives
/// liveness from the runner's status record.
#[tokio::test]
async fn runtimes_are_admin_written_and_read_with_liveness() {
    let p = gateway().await;
    let token = admin(&p);
    let page = "markdown/instances/runtime/laptop.md";
    let content = "---\nkind: instance\nskill: runtime\nid: laptop\nrunner_id: \"laptop\"\n\
        harnesses: [{\"name\": \"echo\", \"version\": \"echo 1.0\"}]\n\
        labels: [\"site:muc\"]\nmax_concurrent: 2\n---\n# laptop\n";
    let member = p.mint_token_with_sub(TENANT, Role::Agent, "member:alice");
    let refused = rpc(
        &p,
        &member,
        "update_page",
        json!({ "page_id": page, "content": content }),
    )
    .await;
    assert!(
        refused.get("error").is_some() || refused["result"]["isError"] == true,
        "a member cannot register a runtime: {refused}"
    );
    call(
        &p,
        &token,
        "update_page",
        json!({ "page_id": page, "content": content }),
    )
    .await;

    let listed = call(&p, &member, "list_runtimes", json!({})).await;
    let rt = &listed["runtimes"][0];
    assert_eq!(rt["runner_id"], "laptop", "{listed}");
    assert_eq!(rt["status"], "unknown", "no status record yet: {rt}");
    assert_eq!(
        rt["harnesses"][0],
        json!({ "name": "echo", "version": "echo 1.0" })
    );
    assert_eq!(rt["labels"], json!(["site:muc"]));
    assert_eq!(rt["max_concurrent"], 2);

    let report = |state: &str| {
        json!({ "runner_id": "laptop", "state": state, "status_interval_s": 1,
                "body": json!({ "version": "9.9.9", "live_runs": [{"run_id": "r1"}] }).to_string() })
    };
    call(&p, &token, "report_runner_status", report("started")).await;
    let rt = call(&p, &member, "list_runtimes", json!({})).await["runtimes"][0].clone();
    assert_eq!(rt["status"], "online", "{rt}");
    assert_eq!(rt["stale_after_s"], 3, "{rt}");
    assert_eq!(rt["version"], "9.9.9", "{rt}");
    assert_eq!(rt["live_runs"], 1, "{rt}");
    assert!(rt["last_seen"].as_str().is_some(), "{rt}");

    tokio::time::sleep(Duration::from_millis(4200)).await;
    let rt = call(&p, &member, "list_runtimes", json!({})).await["runtimes"][0].clone();
    assert_eq!(rt["status"], "stale", "silent past 3 x interval: {rt}");

    call(&p, &token, "report_runner_status", report("stopping")).await;
    let rt = call(&p, &member, "list_runtimes", json!({})).await["runtimes"][0].clone();
    assert_eq!(rt["status"], "stopped", "{rt}");
}

/// A gateway restart forgets every lease (crew F6). For the boot grace, an
/// event captured before the restart cannot be claimed by another runner —
/// only its holder's renew re-creates the lease — while an event captured
/// after the restart is claimable at once.
#[tokio::test]
async fn after_a_restart_a_forgotten_lease_goes_back_to_its_holder_not_to_a_second_runner() {
    let first = gateway().await;
    let in_flight = capture(&first, "note").await;
    let token = admin(&first);
    let a = json!({ "event_id": in_flight, "holder_id": "runner-a", "lease_secs": 600 });
    assert_eq!(
        call(&first, &token, "claim_event", a.clone()).await["claimed"],
        true
    );

    // Restart: same tenant data, a new process with an empty claims table.
    let data = first.indexer().expect("the gateway's indexer");
    first.shutdown().await;
    let p = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        config_overrides: escurel_test_support::ConfigOverrides {
            indexer: Some(data),
            ..Default::default()
        },
        ..Default::default()
    })
    .await;
    let token = admin(&p);
    let b = json!({ "event_id": in_flight, "holder_id": "runner-b", "lease_secs": 600 });

    let refused = call(&p, &token, "claim_event", b.clone()).await;
    assert_eq!(refused["claimed"], false, "{refused}");
    assert_eq!(refused["holder"], "(recovering)", "{refused}");

    let renewed = call(&p, &token, "renew_claim", a).await;
    assert_eq!(renewed["renewed"], true, "{renewed}");
    let still = call(&p, &token, "claim_event", b).await;
    assert_eq!(still["claimed"], false, "{still}");
    assert!(
        still["holder"]
            .as_str()
            .is_some_and(|h| h.ends_with("#runner-a")),
        "{still}"
    );

    let fresh = capture(&p, "note").await;
    let won = call(
        &p,
        &token,
        "claim_event",
        json!({ "event_id": fresh, "holder_id": "runner-b" }),
    )
    .await;
    assert_eq!(
        won["claimed"], true,
        "a post-restart event is not held: {won}"
    );
}
