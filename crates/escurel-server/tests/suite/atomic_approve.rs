//! Atomic approve for held writes (issue #354, narrowed).
//!
//! An app that holds a `review`/`confirm` write (the `autonomy:` gate)
//! approves it later — and the approve→commit must be a CAS against
//! the state the proposal was drafted on, or approval silently
//! overwrites an edit that landed in between.
//!
//! `base_version` (CRDT gateways) already does this. The gap this pins
//! shut: **plain gateways had no guard at all** — worse, a caller
//! sending a guard arg the server didn't know was silently dropped and
//! the write went through UNGUARDED. `base_sha256` is the guard that
//! works everywhere: the hex sha256 of the stored markdown the
//! proposal was drafted against ("" = "I expect no page yet" for
//! approve-create). A mismatch refuses `{code: conflict}` carrying
//! `head_sha256` (+ `head_content`) so the approver re-diffs without a
//! second race.

use escurel_test_support::{AuthMode, ConfigOverrides, EscurelProcess, FixtureBuilder, Opts, Role};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

const TENANT: &str = "stuttgart-ai";
const NOTE_SKILL: &str = "---\ntype: skill\nid: note\ndescription: A note.\n\
    visibility: public\n---\n# note\n";
const DRAFT_BASE: &str = "---\ntype: instance\nskill: note\nid: plan\n---\n# Plan\nv1 body.\n";
const PAGE: &str = "markdown/instances/note/plan.md";

fn sha(s: &str) -> String {
    format!("{:x}", Sha256::digest(s.as_bytes()))
}

async fn start() -> EscurelProcess {
    EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        config_overrides: ConfigOverrides::default(),
        fixtures: Some(
            FixtureBuilder::new()
                .tenant(TENANT)
                .skill("note", NOTE_SKILL)
                .instance("note", "plan", DRAFT_BASE)
                .done(),
        ),
    })
    .await
}

async fn update(p: &EscurelProcess, token: &str, args: Value) -> Value {
    let body: Value = reqwest::Client::new()
        .post(p.mcp_url())
        .header("authorization", format!("Bearer {token}"))
        .json(&json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": { "name": "update_page", "arguments": args },
        }))
        .send()
        .await
        .expect("post")
        .json()
        .await
        .expect("json");
    assert!(body.get("error").is_none(), "update_page error: {body}");
    body["result"]["structuredContent"].clone()
}

/// The full approve lifecycle on a PLAIN gateway (no CRDT backend —
/// exactly where `base_version` answers `versioning_unavailable`).
#[tokio::test]
async fn approve_by_hash_is_a_real_cas_on_a_plain_gateway() {
    let p = start().await;
    let token = p.mint_token(TENANT, Role::Agent);

    let approved = "---\ntype: instance\nskill: note\nid: plan\n---\n# Plan\nAPPROVED body.\n";

    // A guard drafted against the WRONG state refuses with the head, so
    // the approver can re-diff — never a silent overwrite.
    let stale = update(
        &p,
        &token,
        json!({
            "page_id": PAGE,
            "content": approved,
            "base_sha256": sha("something the reviewer never saw"),
        }),
    )
    .await;
    assert_eq!(stale["ok"], json!(false), "stale hash must refuse: {stale}");
    assert_eq!(stale["issues"][0]["code"], json!("conflict"), "{stale}");
    assert_eq!(
        stale["head_sha256"],
        json!(sha(DRAFT_BASE)),
        "the refusal carries the current head hash: {stale}"
    );
    assert!(
        stale["head_content"]
            .as_str()
            .is_some_and(|c| c.contains("v1 body")),
        "and the head content for the re-diff: {stale}"
    );

    // The guard drafted against the REAL state commits.
    let ok = update(
        &p,
        &token,
        json!({
            "page_id": PAGE,
            "content": approved,
            "base_sha256": sha(DRAFT_BASE),
        }),
    )
    .await;
    assert_eq!(ok["ok"], json!(true), "correct hash commits: {ok}");

    // And the previous guard is now stale — the CAS moved with the head.
    let replay = update(
        &p,
        &token,
        json!({
            "page_id": PAGE,
            "content": approved,
            "base_sha256": sha(DRAFT_BASE),
        }),
    )
    .await;
    assert_eq!(
        replay["ok"],
        json!(false),
        "an approval replayed after the commit is a conflict, not a \
         second write: {replay}"
    );
}

/// Approve-create: `base_sha256: ""` means "I expect no page yet".
#[tokio::test]
async fn approve_create_guards_against_a_page_appearing_in_between() {
    let p = start().await;
    let token = p.mint_token(TENANT, Role::Agent);
    const NEW: &str = "markdown/instances/note/fresh.md";
    let content = "---\ntype: instance\nskill: note\nid: fresh\n---\n# Fresh\n";

    // Nothing there yet → the empty-guard create commits.
    let ok = update(
        &p,
        &token,
        json!({ "page_id": NEW, "content": content, "base_sha256": "" }),
    )
    .await;
    assert_eq!(ok["ok"], json!(true), "guarded create lands: {ok}");

    // A second guarded create finds a page where none was expected.
    let raced = update(
        &p,
        &token,
        json!({ "page_id": NEW, "content": content, "base_sha256": "" }),
    )
    .await;
    assert_eq!(
        raced["ok"],
        json!(false),
        "the page appeared in between — conflict, not overwrite: {raced}"
    );
    assert_eq!(raced["issues"][0]["code"], json!("conflict"), "{raced}");
}

/// The CRDT-gateway guard's conflict now carries the head version as a
/// STRUCTURED field (it was only inside the English message).
#[tokio::test]
async fn version_conflict_carries_the_head_version_structurally() {
    let p = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        config_overrides: ConfigOverrides {
            live_crdt: true,
            ..Default::default()
        },
        fixtures: Some(
            FixtureBuilder::new()
                .tenant(TENANT)
                .skill("note", NOTE_SKILL)
                .instance("note", "plan", DRAFT_BASE)
                .done(),
        ),
    })
    .await;
    let token = p.mint_token(TENANT, Role::Agent);

    // Advance the head once so a made-up base is stale.
    let first = update(
        &p,
        &token,
        json!({ "page_id": PAGE, "content": DRAFT_BASE.replace("v1", "v2") }),
    )
    .await;
    assert_eq!(first["ok"], json!(true), "{first}");

    let stale = update(
        &p,
        &token,
        json!({
            "page_id": PAGE,
            "content": DRAFT_BASE.replace("v1", "v3"),
            "base_version": "v0",
            "require_exact_base": true,
        }),
    )
    .await;
    assert_eq!(stale["ok"], json!(false), "{stale}");
    assert!(
        stale["head_version"].is_string(),
        "the conflict names the head version structurally: {stale}"
    );
}

/// The read half of the approve loop: `expand` publishes
/// `content_sha256` — the hash of the STORED markdown bytes, i.e.
/// exactly the value `update_page`'s `base_sha256` guard compares
/// against — so a client can hold "what the drafter saw" without a
/// write-probe or byte-perfect reconstruction from parsed fields.
#[tokio::test]
async fn expand_publishes_the_guard_hash() {
    let p = start().await;
    let token = p.mint_token(TENANT, Role::Agent);

    let body: Value = reqwest::Client::new()
        .post(p.mcp_url())
        .header("authorization", format!("Bearer {token}"))
        .json(&json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": { "name": "expand", "arguments": { "page_id": PAGE } },
        }))
        .send()
        .await
        .expect("post")
        .json()
        .await
        .expect("json");
    let s = &body["result"]["structuredContent"];
    assert_eq!(
        s["content_sha256"],
        json!(sha(DRAFT_BASE)),
        "expand names the stored-bytes hash: {body}"
    );

    // And it IS the guard: approving with it commits first time.
    let ok = update(
        &p,
        &token,
        json!({
            "page_id": PAGE,
            "content": DRAFT_BASE.replace("v1", "approved"),
            "base_sha256": s["content_sha256"],
        }),
    )
    .await;
    assert_eq!(ok["ok"], json!(true), "{ok}");
}
