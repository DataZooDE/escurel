//! Schema-ergonomics contract over real HTTP (2026-08-14 API review,
//! consumer-ergonomics findings F1/F4 + naming B5/B9).
//!
//! Three classes of trap this pins shut:
//! - `capture_event` accepted `{}` and minted an unroutable junk event
//!   (empty `label_skill` = nothing for the runner to route on);
//! - sibling tools spelled the same concept differently (`skill` on
//!   `search` vs `skill_id` on `list_instances`; `from`/`to` vs
//!   `from_page_id`/`to_page_id`) and unknown args are silently
//!   dropped, so the WRONG spelling "succeeded" with default behaviour;
//! - a handful of admin write envelopes omitted `ok`, so their
//!   refusals could never reach MCP `isError`.

use escurel_test_support::{AuthMode, ConfigOverrides, EscurelProcess, FixtureBuilder, Opts, Role};
use serde_json::{Value, json};

const TENANT: &str = "stuttgart-ai";
const NOTE_SKILL: &str = "---\ntype: skill\nid: note\ndescription: A note.\n\
    visibility: public\n---\n# note\n";
const NOTE_A: &str = "---\ntype: instance\nskill: note\nid: a\n---\n# A\n";
const EVOLVE_PROBLEM_SKILL: &str = "---\ntype: skill\nid: evolve_problem\ndescription: Evolve problem.\nvisibility: public\n---\n# Evolve problem\n";
const EVOLVE_PROBLEM_A: &str = "---\ntype: instance\nskill: evolve_problem\nid: a\n---\n# A\n";
const PRIVATE_EVOLVE_PROBLEM_SKILL: &str = "---\ntype: skill\nid: evolve_problem\ndescription: Owner problem.\nowner_field: owner_subject\nacl:\n  read: [owner]\n  create: [owner]\n  update: [owner]\n---\n# Evolve problem\n";
const PRIVATE_EVOLVE_PROBLEM_A: &str =
    "---\ntype: instance\nskill: evolve_problem\nid: a\nowner_subject: test-subject\n---\n# A\n";
const EVOLVE_EXPERIMENT_SKILL: &str = "---\ntype: skill\nid: evolve_experiment\ndescription: Evolve experiment.\nowner_field: owner_subject\nacl:\n  read: [owner]\n  create: [admin]\n  update: [admin]\n---\n# Evolve experiment\n";
const EVOLVE_EXPERIMENT_A: &str = "---\ntype: instance\nskill: evolve_experiment\nid: a\nowner_subject: test-subject\nstatus: completed\nbest_program_id: 7\nnext_validation_action: evolve_validate_winner\n---\n# A\n";
const EVOLVE_REPORT_SKILL: &str = "---\ntype: skill\nid: evolve_validation_report\ndescription: Evolve private report.\nowner_field: owner_subject\nacl:\n  read: [owner]\n  create: [admin]\n  update: [admin]\n---\n# Evolve report\n";
const EVOLVE_REPORT_A: &str = "---\ntype: instance\nskill: evolve_validation_report\nid: a\nowner_subject: test-subject\nstatus: passed\neffective_passed: true\nwinner_program_id: 7\nreport_sha256: aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\nnext_candidate_action: evolve_publish_candidate\n---\n# Report\n";

async fn start() -> EscurelProcess {
    EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        config_overrides: ConfigOverrides::default(),
        fixtures: Some(
            FixtureBuilder::new()
                .tenant(TENANT)
                .skill("note", NOTE_SKILL)
                .instance("note", "a", NOTE_A)
                .done(),
        ),
    })
    .await
}

async fn call(p: &EscurelProcess, token: &str, name: &str, args: Value) -> Value {
    reqwest::Client::new()
        .post(p.mcp_url())
        .header("authorization", format!("Bearer {token}"))
        .json(&json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": { "name": name, "arguments": args },
        }))
        .send()
        .await
        .expect("post")
        .json()
        .await
        .expect("decode")
}

/// An event with no `label_skill` is unroutable — the runner selects
/// its system prompt by that label. `{}` used to succeed silently.
#[tokio::test]
async fn capture_event_requires_a_label_skill() {
    let p = start().await;
    let token = p.mint_token(TENANT, Role::Agent);

    let refused = call(&p, &token, "capture_event", json!({ "source": "t" })).await;
    assert_eq!(
        refused["error"]["code"],
        json!(-32602),
        "an unlabelled capture must be refused, not minted as junk: {refused}"
    );

    let ok = call(
        &p,
        &token,
        "capture_event",
        json!({ "source": "t", "label_skill": "note" }),
    )
    .await;
    assert!(ok.get("error").is_none(), "a labelled capture lands: {ok}");
}

#[tokio::test]
async fn evolve_run_capture_checks_and_stamps_the_exact_problem_revision() {
    use sha2::{Digest, Sha256};

    let p = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        config_overrides: ConfigOverrides::default(),
        fixtures: Some(
            FixtureBuilder::new()
                .tenant(TENANT)
                .skill("note", NOTE_SKILL)
                .instance("note", "a", NOTE_A)
                .skill("evolve_problem", EVOLVE_PROBLEM_SKILL)
                .instance("evolve_problem", "a", EVOLVE_PROBLEM_A)
                .done(),
        ),
    })
    .await;
    let token = p.mint_token(TENANT, Role::Admin);
    let catalog = call(&p, &token, "list_skills", json!({})).await;
    assert_eq!(
        catalog["result"]["structuredContent"]["evolve_revision_binding"],
        "gateway-owned-v2"
    );
    let target = "markdown/instances/evolve_problem/a.md";
    let hash = format!("{:x}", Sha256::digest(EVOLVE_PROBLEM_A.as_bytes()));
    let page = call(&p, &token, "expand", json!({"page_id": target})).await;
    assert!(page.get("error").is_none(), "{page}");
    assert_eq!(page["result"]["structuredContent"]["content_sha256"], hash);
    let request = |expected: &str| {
        json!({
            "label_skill": "evolve_run",
            "event_id": "EVOLVE-ACTION-1",
            "instance_page_id": target,
            "source": "workbench",
            "provenance": {"manual": {
                "mode": "run",
                "expected_page_sha256": expected,
                "target_page_sha256": "forged",
                "target_page_sha256_gateway_verified": false
            }}
        })
    };

    let stale = call(&p, &token, "capture_event", request(&"0".repeat(64))).await;
    assert_eq!(stale["error"]["code"], json!(-32602), "{stale}");

    let mut wrong_type = request(&format!("{:x}", Sha256::digest(NOTE_A.as_bytes())));
    wrong_type["instance_page_id"] = json!("markdown/instances/note/a.md");
    let wrong_type = call(&p, &token, "capture_event", wrong_type).await;
    assert_eq!(wrong_type["error"]["code"], json!(-32602), "{wrong_type}");

    let accepted = call(&p, &token, "capture_event", request(&hash)).await;
    assert!(accepted.get("error").is_none(), "{accepted}");
    assert_eq!(
        accepted["result"]["structuredContent"]["provenance"]["manual"]["target_page_sha256"],
        hash
    );
    assert_eq!(
        accepted["result"]["structuredContent"]["provenance"]["manual"]["target_page_sha256_gateway_verified"],
        true
    );
    assert_eq!(
        accepted["result"]["structuredContent"]["provenance"]["manual"]["requested_by"],
        "test-subject"
    );
    let attested = call(
        &p,
        &token,
        "list_events",
        json!({"event_id": "EVOLVE-ACTION-1"}),
    )
    .await;
    assert_eq!(
        attested["result"]["structuredContent"]["events"][0]["revision_binding_attested"],
        true
    );
    let preempted = call(
        &p,
        &token,
        "capture_event",
        json!({"event_id": "EVOLVE-PREEMPTED", "label_skill": "note",
               "instance_page_id": "markdown/instances/note/a.md"}),
    )
    .await;
    assert!(preempted.get("error").is_none(), "{preempted}");
    let mut collided = request(&hash);
    collided["event_id"] = json!("EVOLVE-PREEMPTED");
    let collided = call(&p, &token, "capture_event", collided).await;
    assert!(collided.get("error").is_none(), "{collided}");
    let row = call(
        &p,
        &token,
        "list_events",
        json!({"event_id": "EVOLVE-PREEMPTED"}),
    )
    .await;
    assert_eq!(
        row["result"]["structuredContent"]["events"][0]["label_skill"],
        "note"
    );
    let changed = call(
        &p,
        &token,
        "update_page",
        json!({
            "page_id": target,
            "content": format!("{EVOLVE_PROBLEM_A}\nEdited after capture.\n"),
            "base_sha256": hash
        }),
    )
    .await;
    assert!(changed.get("error").is_none(), "{changed}");
    let edited_page = call(&p, &token, "expand", json!({"page_id": target})).await;
    assert!(edited_page.get("error").is_none(), "{edited_page}");
    let retry = call(&p, &token, "capture_event", request(&hash)).await;
    assert!(retry.get("error").is_none(), "{retry}");
    assert_eq!(
        retry["result"]["structuredContent"]["event_id"],
        "EVOLVE-ACTION-1"
    );
}

#[tokio::test]
async fn evolve_preflight_capture_is_private_and_revision_bound() {
    use sha2::{Digest, Sha256};

    let p = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(
            FixtureBuilder::new()
                .tenant(TENANT)
                .skill("evolve_problem", PRIVATE_EVOLVE_PROBLEM_SKILL)
                .instance("evolve_problem", "a", PRIVATE_EVOLVE_PROBLEM_A)
                .done(),
        ),
        ..Default::default()
    })
    .await;
    let owner = p.mint_token(TENANT, Role::Agent);
    let other = p.mint_token_with_sub(TENANT, Role::Agent, "other-user");
    let catalog = call(&p, &owner, "list_skills", json!({})).await;
    assert_eq!(
        catalog["result"]["structuredContent"]["evolve_preflight_revision_binding"],
        "gateway-owned-v1"
    );
    let target = "markdown/instances/evolve_problem/a.md";
    let hash = format!("{:x}", Sha256::digest(PRIVATE_EVOLVE_PROBLEM_A.as_bytes()));
    let request = |revision: &str| {
        json!({
            "label_skill": "evolve_preflight", "event_id": "EVOLVE-PREFLIGHT-A",
            "instance_page_id": target, "source": "workbench",
            "provenance": {"manual": {"mode": "run", "expected_page_sha256": revision}}
        })
    };
    let stale = call(&p, &owner, "capture_event", request(&"0".repeat(64))).await;
    assert_eq!(stale["error"]["code"], -32602, "{stale}");
    let denied = call(&p, &other, "capture_event", request(&hash)).await;
    assert_eq!(denied["error"]["code"], -32602, "{denied}");
    let accepted = call(&p, &owner, "capture_event", request(&hash)).await;
    assert!(accepted.get("error").is_none(), "{accepted}");
    let row = call(
        &p,
        &owner,
        "list_events",
        json!({"event_id": "EVOLVE-PREFLIGHT-A"}),
    )
    .await;
    assert_eq!(
        row["result"]["structuredContent"]["events"][0]["revision_binding_attested"],
        true
    );
    assert_eq!(
        row["result"]["structuredContent"]["events"][0]["status"],
        "processed"
    );
    let hidden = call(
        &p,
        &other,
        "list_events",
        json!({"event_id": "EVOLVE-PREFLIGHT-A"}),
    )
    .await;
    assert!(
        hidden["result"]["structuredContent"]["events"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let inbox = call(&p, &owner, "list_inbox", json!({})).await;
    assert!(
        inbox["result"]["structuredContent"]["events"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn evolve_validation_capture_binds_the_displayed_winner_and_revision() {
    use sha2::{Digest, Sha256};

    let p = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(
            FixtureBuilder::new()
                .tenant(TENANT)
                .skill("evolve_experiment", EVOLVE_EXPERIMENT_SKILL)
                .instance("evolve_experiment", "a", EVOLVE_EXPERIMENT_A)
                .done(),
        ),
        ..Default::default()
    })
    .await;
    let token = p.mint_token(TENANT, Role::Agent);
    let catalog = call(&p, &token, "list_skills", json!({})).await;
    assert_eq!(
        catalog["result"]["structuredContent"]["evolve_validation_revision_binding"],
        "gateway-owned-v1"
    );
    let target = "markdown/instances/evolve_experiment/a.md";
    let hash = format!("{:x}", Sha256::digest(EVOLVE_EXPERIMENT_A.as_bytes()));
    let request = |winner: u64, revision: &str| {
        json!({
            "label_skill": "evolve_validate", "event_id": "EVOLVE-VALIDATE-A",
            "instance_page_id": target, "source": "workbench",
            "provenance": {"manual": {"mode": "run", "expected_page_sha256": revision,
                "expected_winner_program_id": winner}}
        })
    };
    let wrong_winner = call(&p, &token, "capture_event", request(8, &hash)).await;
    assert_eq!(wrong_winner["error"]["code"], -32602, "{wrong_winner}");
    let stale = call(&p, &token, "capture_event", request(7, &"0".repeat(64))).await;
    assert_eq!(stale["error"]["code"], -32602, "{stale}");
    let accepted = call(&p, &token, "capture_event", request(7, &hash)).await;
    assert!(accepted.get("error").is_none(), "{accepted}");
    let stored = call(
        &p,
        &token,
        "list_events",
        json!({"event_id": "EVOLVE-VALIDATE-A"}),
    )
    .await;
    assert_eq!(
        stored["result"]["structuredContent"]["events"][0]["revision_binding_attested"],
        true
    );
    assert_eq!(
        stored["result"]["structuredContent"]["events"][0]["provenance"]["manual"]["target_page_sha256"],
        hash
    );
    assert_eq!(
        stored["result"]["structuredContent"]["events"][0]["status"],
        "processed"
    );
    let inbox = call(&p, &token, "list_inbox", json!({})).await;
    assert!(
        inbox["result"]["structuredContent"]["events"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let other_token = p.mint_token_with_sub(TENANT, Role::Agent, "other-user");
    let mut other_request = request(7, &hash);
    other_request["event_id"] = json!("EVOLVE-VALIDATE-OTHER");
    let denied = call(&p, &other_token, "capture_event", other_request).await;
    assert_eq!(denied["error"]["code"], -32602, "{denied}");
    let mut collided = request(7, &hash);
    collided["event_id"] = json!("EVOLVE-VALIDATE-A");
    let denied_collision = call(&p, &other_token, "capture_event", collided).await;
    assert_eq!(
        denied_collision["error"]["code"], -32602,
        "{denied_collision}"
    );
}

#[tokio::test]
async fn evolve_candidate_capture_requires_owner_confirmation_and_exact_report() {
    use sha2::{Digest, Sha256};

    let p = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(
            FixtureBuilder::new()
                .tenant(TENANT)
                .skill("evolve_validation_report", EVOLVE_REPORT_SKILL)
                .instance("evolve_validation_report", "a", EVOLVE_REPORT_A)
                .done(),
        ),
        ..Default::default()
    })
    .await;
    let token = p.mint_token(TENANT, Role::Agent);
    let catalog = call(&p, &token, "list_skills", json!({})).await;
    assert_eq!(
        catalog["result"]["structuredContent"]["evolve_candidate_revision_binding"],
        "gateway-owned-v1"
    );
    let target = "markdown/instances/evolve_validation_report/a.md";
    let hash = format!("{:x}", Sha256::digest(EVOLVE_REPORT_A.as_bytes()));
    let request = |winner: u64, revision: &str, report_hash: &str, confirm: bool| {
        json!({
            "label_skill": "evolve_publish_candidate", "event_id": "EVOLVE-CANDIDATE-A",
            "instance_page_id": target, "source": "workbench",
            "provenance": {"manual": {"mode": "run", "expected_page_sha256": revision,
                "expected_winner_program_id": winner,
                "expected_validation_report_sha256": report_hash,
                "confirm": confirm, "review_note": "reviewed two tails"}}
        })
    };
    for rejected in [
        request(8, &hash, &"a".repeat(64), true),
        request(7, &"0".repeat(64), &"a".repeat(64), true),
        request(7, &hash, &"b".repeat(64), true),
        request(7, &hash, &"a".repeat(64), false),
    ] {
        let response = call(&p, &token, "capture_event", rejected).await;
        assert_eq!(response["error"]["code"], -32602, "{response}");
    }
    let accepted = call(
        &p,
        &token,
        "capture_event",
        request(7, &hash, &"a".repeat(64), true),
    )
    .await;
    assert!(accepted.get("error").is_none(), "{accepted}");
    let stored = call(
        &p,
        &token,
        "list_events",
        json!({"event_id": "EVOLVE-CANDIDATE-A"}),
    )
    .await;
    assert_eq!(
        stored["result"]["structuredContent"]["events"][0]["revision_binding_attested"],
        true
    );
    assert_eq!(
        stored["result"]["structuredContent"]["events"][0]["status"],
        "processed"
    );
    let inbox = call(&p, &token, "list_inbox", json!({})).await;
    assert!(
        inbox["result"]["structuredContent"]["events"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let other = p.mint_token_with_sub(TENANT, Role::Agent, "other-user");
    let mut other_request = request(7, &hash, &"a".repeat(64), true);
    other_request["event_id"] = json!("EVOLVE-CANDIDATE-OTHER");
    let denied = call(&p, &other, "capture_event", other_request).await;
    assert_eq!(denied["error"]["code"], -32602, "{denied}");
}

/// `search` filters by `skill`, `list_instances` by `skill_id` — real
/// wire divergence, and the wrong spelling was silently dropped. Each
/// now accepts the sibling's spelling as an alias.
#[tokio::test]
async fn skill_id_and_skill_alias_each_other() {
    let p = start().await;
    let token = p.mint_token(TENANT, Role::Agent);

    // list_instances with the `skill` spelling (search's) must work.
    let li = call(&p, &token, "list_instances", json!({ "skill": "note" })).await;
    assert!(li.get("error").is_none(), "alias `skill` accepted: {li}");
    let instances = li["result"]["structuredContent"]["instances"]
        .as_array()
        .unwrap_or_else(|| panic!("instances shape: {li}"));
    assert_eq!(instances.len(), 1, "the note instance is found: {li}");

    // search with the `skill_id` spelling (list_instances') must not
    // error (hits may be empty — ZeroEmbedder — but the arg must bind).
    let se = call(
        &p,
        &token,
        "search",
        json!({ "q": "a", "skill_id": "note" }),
    )
    .await;
    assert!(se.get("error").is_none(), "alias `skill_id` accepted: {se}");
}

/// `move_page` spelled its pair `from`/`to` while `provenance_path`
/// says `from_page`/`to_page` — the long spellings now alias.
#[tokio::test]
async fn move_page_accepts_the_long_page_pair_spelling() {
    let p = start().await;
    let token = p.mint_token(TENANT, Role::Agent);
    let moved = call(
        &p,
        &token,
        "move_page",
        json!({
            "from_page_id": "markdown/instances/note/a.md",
            "to_page_id": "markdown/instances/note/b.md",
        }),
    )
    .await;
    assert!(
        moved.get("error").is_none(),
        "long spellings must bind, not vanish into the unknown-arg void: {moved}"
    );
    assert_eq!(
        moved["result"]["structuredContent"]["ok"],
        json!(true),
        "the move lands: {moved}"
    );
}

/// `admin_delete_chat_history` returned bare `{deleted}` — an envelope
/// that can never carry `ok:false`, so MCP `isError` was unreachable
/// for it. Success now says `ok: true` like its admin siblings.
#[tokio::test]
async fn admin_chat_purge_envelope_carries_ok() {
    let p = start().await;
    let admin = p.mint_token(TENANT, Role::Admin);
    let out = call(
        &p,
        &admin,
        "admin_delete_chat_history",
        json!({ "chat_group_id": "nobody" }),
    )
    .await;
    let s = &out["result"]["structuredContent"];
    assert_eq!(s["ok"], json!(true), "envelope carries ok: {out}");
    assert!(
        s["deleted"].is_number(),
        "payload unchanged beside it: {out}"
    );
}

/// A schema that under-declares what its handler accepts is worse than a
/// missing schema: a client generated from `tools/list` cannot ask for the
/// thing, and the handler's own error message names it as valid.
///
/// `list_lineage`'s `include` accepted `tool_calls` — the only way to get
/// `tool_call_summary` onto a run node, which is what a thread view needs to
/// show "14 tool calls" without a call per run — while declaring only
/// `events | runs | drafts`.
#[tokio::test]
async fn list_lineage_declares_every_include_its_handler_accepts() {
    let p = start().await;
    let token = p.mint_token(TENANT, Role::Agent);

    let listed: Value = reqwest::Client::new()
        .post(p.mcp_url())
        .header("authorization", format!("Bearer {token}"))
        .json(&json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" }))
        .send()
        .await
        .expect("post")
        .json()
        .await
        .expect("decode");
    let declared: Vec<String> = listed["result"]["tools"]
        .as_array()
        .expect("tools")
        .iter()
        .find(|t| t["name"] == json!("list_lineage"))
        .expect("list_lineage is listed")["inputSchema"]["properties"]["include"]["items"]["enum"]
        .as_array()
        .expect("include.items.enum")
        .iter()
        .map(|v| v.as_str().unwrap_or_default().to_owned())
        .collect();

    // Every declared value is accepted: a schema may not promise what the
    // handler refuses either.
    for value in &declared {
        let out = call(
            &p,
            &token,
            "list_lineage",
            json!({ "root_event_id": "01HNOSUCHROOT", "include": [value] }),
        )
        .await;
        assert!(
            out.get("error").is_none(),
            "the schema declares `{value}` but the handler refuses it: {out}"
        );
    }

    // …and every accepted value is declared. `tool_calls` is the one the
    // handler names in its own refusal message, so it is not a guess.
    let accepted = call(
        &p,
        &token,
        "list_lineage",
        json!({ "root_event_id": "01HNOSUCHROOT", "include": ["tool_calls"] }),
    )
    .await;
    assert!(
        accepted.get("error").is_none(),
        "`tool_calls` must stay accepted: {accepted}"
    );
    assert!(
        declared.iter().any(|v| v == "tool_calls"),
        "the handler accepts `tool_calls`, so the schema must declare it; declared: {declared:?}"
    );
}
