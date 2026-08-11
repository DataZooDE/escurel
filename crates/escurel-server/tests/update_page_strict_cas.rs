//! The two halves of the optimistic-concurrency **contract**, as opposed to
//! its mechanism (`update_page_automerge.rs` covers the merge itself):
//!
//! 1. `require_exact_base` — a caller who cannot accept a merged document says
//!    so, and gets a conflict with nothing persisted;
//! 2. `versioning_unavailable` — a caller who asks for the guard on a gateway
//!    that cannot provide it is refused, not silently written unguarded.
//!
//! Both were found from downstream (Heron): a human-approval flow shipped
//! `base_version` on every approval, ran against a gateway with no CRDT
//! backend, and had the guard discarded server-side — so approving a proposal
//! whose target had moved clobbered the concurrent edit and reported success.
//!
//! No mocks: a real `EscurelProcess`, real DuckDB, real `/mcp`.

use escurel_test_support::{AuthMode, ConfigOverrides, EscurelProcess, FixtureBuilder, Opts, Role};
use serde_json::{Value, json};

const TENANT: &str = "acme";
const CUSTOMER: &str = "---\ntype: skill\nid: customer\ndescription: x\n---\n# customer\n";
const C1: &str = "---\ntype: instance\nskill: customer\nid: c1\n---\n# Acme\n\nseed.\n";
const PAGE: &str = "markdown/instances/customer/c1.md";

fn page(body: &str) -> String {
    format!("---\ntype: instance\nskill: customer\nid: c1\n---\n# Acme\n\n{body}\n")
}

async fn start(live_crdt: bool) -> EscurelProcess {
    EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(
            FixtureBuilder::new()
                .tenant(TENANT)
                .skill("customer", CUSTOMER)
                .instance("customer", "c1", C1)
                .done(),
        ),
        config_overrides: ConfigOverrides {
            live_crdt,
            ..Default::default()
        },
    })
    .await
}

async fn call(p: &EscurelProcess, name: &str, args: Value) -> Value {
    let token = p.mint_token(TENANT, Role::Agent);
    let body: Value = reqwest::Client::new()
        .post(p.mcp_url())
        .header("authorization", format!("Bearer {token}"))
        .json(&json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": { "name": name, "arguments": args } }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    body["result"]["structuredContent"].clone()
}

fn issue_codes(v: &Value) -> Vec<&str> {
    v["issues"]
        .as_array()
        .map(|a| a.iter().filter_map(|i| i["code"].as_str()).collect())
        .unwrap_or_default()
}

/// `live_crdt` is the harness's way of booting the shape the binary always
/// boots. If it does not actually wire a backend, every assertion in this file
/// is vacuous, so it is pinned first and on its own.
#[tokio::test]
async fn live_crdt_publishes_a_readable_head_version() {
    let p = start(true).await;

    let before = call(&p, "expand", json!({ "page_id": PAGE })).await;
    assert_eq!(
        before["version"], "v0",
        "a seeded page's head must be readable from expand: {before}"
    );

    let write = call(&p, "update_page", json!({ "page_id": PAGE, "content": page("one") })).await;
    assert_eq!(write["ok"], true, "{write}");
    let after = call(&p, "expand", json!({ "page_id": PAGE })).await;
    assert_eq!(
        after["version"], write["new_version"],
        "the version a read publishes must be the one the write reported — a \
         client drafting from a read would otherwise be stale on arrival"
    );
    assert_ne!(
        after["version"], before["version"],
        "control: the head must actually advance, or the equality above holds \
         for a constant"
    );

    p.shutdown().await;
}

/// A strict caller gets a conflict on a stale base, and — the part an error
/// code alone does not promise — **nothing is written**.
#[tokio::test]
async fn require_exact_base_conflicts_instead_of_merging() {
    let p = start(true).await;

    let first = call(
        &p,
        "update_page",
        json!({ "page_id": PAGE, "content": page("line one\n\nline two") }),
    )
    .await;
    let base = first["new_version"].as_str().unwrap().to_owned();

    // Someone else edits a DIFFERENT region — the shape that merges cleanly.
    let concurrent = call(
        &p,
        "update_page",
        json!({
            "page_id": PAGE,
            "content": page("line one\n\nline two EDITED BY SOMEONE ELSE"),
            "base_version": base,
        }),
    )
    .await;
    assert_eq!(concurrent["ok"], true, "{concurrent}");

    // CONTROL for the merge path: the same stale write WITHOUT the flag is
    // merged and persisted. This is what makes the strict result below a
    // decision rather than an accident of unmergeable content.
    let lenient = call(
        &p,
        "update_page",
        json!({
            "page_id": PAGE,
            "content": page("line one MERGEABLE\n\nline two"),
            "base_version": base,
        }),
    )
    .await;
    assert_eq!(lenient["ok"], true, "control: this stale write merges: {lenient}");
    assert_eq!(
        lenient["auto_merged"], true,
        "control: and it merged rather than took the clean path: {lenient}"
    );

    // The strict caller, same staleness, same mergeable shape.
    let head_before = call(&p, "expand", json!({ "page_id": PAGE })).await["body"]
        .as_str()
        .unwrap()
        .to_owned();
    let strict = call(
        &p,
        "update_page",
        json!({
            "page_id": PAGE,
            "content": page("line one STRICT\n\nline two"),
            "base_version": base,
            "require_exact_base": true,
        }),
    )
    .await;
    assert_eq!(strict["ok"], false, "a strict stale write must refuse: {strict}");
    assert_eq!(issue_codes(&strict), vec!["conflict"], "{strict}");
    assert!(
        strict["head_content"].is_string(),
        "the refusal must carry head_content so the caller can re-draft: {strict}"
    );

    let head_after = call(&p, "expand", json!({ "page_id": PAGE })).await["body"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(
        head_before, head_after,
        "a refused strict write must persist NOTHING — not the draft, not a merge"
    );
    assert!(
        !head_after.contains("STRICT"),
        "no part of the refused draft may survive: {head_after}"
    );

    // POSITIVE CONTROL: strict against the CURRENT head still lands.
    let head = call(&p, "expand", json!({ "page_id": PAGE })).await["version"]
        .as_str()
        .unwrap()
        .to_owned();
    let in_date = call(
        &p,
        "update_page",
        json!({
            "page_id": PAGE,
            "content": page("line one IN DATE\n\nline two"),
            "base_version": head,
            "require_exact_base": true,
        }),
    )
    .await;
    assert_eq!(
        in_date["ok"], true,
        "control: an in-date strict write must land, or `require_exact_base` \
         is simply 'refuse everything': {in_date}"
    );
    assert_eq!(in_date["auto_merged"], false, "{in_date}");

    p.shutdown().await;
}

/// `require_exact_base` with nothing to be exact about is a caller bug, and
/// the forgiving reading of it (ignore the flag) is the one that writes
/// unguarded.
#[tokio::test]
async fn require_exact_base_without_a_base_is_rejected() {
    let p = start(true).await;
    let token = p.mint_token(TENANT, Role::Agent);
    let body: Value = reqwest::Client::new()
        .post(p.mcp_url())
        .header("authorization", format!("Bearer {token}"))
        .json(&json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": { "name": "update_page", "arguments": {
                "page_id": PAGE, "content": page("x"), "require_exact_base": true } } }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        body["error"]["code"], -32602,
        "expected invalid_params: {body}"
    );

    let landed = call(&p, "expand", json!({ "page_id": PAGE })).await;
    assert!(
        !landed["body"].as_str().unwrap().contains('x'),
        "the rejected call must not have written: {landed}"
    );

    p.shutdown().await;
}

/// The silent-degradation bug. On a gateway with no version tracking, a
/// `base_version` used to be dropped and the write applied unguarded — the
/// worst answer to a request for a safety check.
#[tokio::test]
async fn asking_for_a_guard_a_gateway_cannot_honour_is_refused() {
    let p = start(false).await;

    let before = call(&p, "expand", json!({ "page_id": PAGE })).await;
    assert!(
        before.get("version").is_none_or(Value::is_null),
        "precondition: this gateway tracks no versions: {before}"
    );

    let guarded = call(
        &p,
        "update_page",
        json!({ "page_id": PAGE, "content": page("guarded"), "base_version": "v0" }),
    )
    .await;
    assert_eq!(guarded["ok"], false, "{guarded}");
    assert_eq!(issue_codes(&guarded), vec!["versioning_unavailable"], "{guarded}");

    let after = call(&p, "expand", json!({ "page_id": PAGE })).await;
    assert!(
        !after["body"].as_str().unwrap().contains("guarded"),
        "the refused write must not have landed: {after}"
    );

    // Same for a guarded delete.
    let del = call(
        &p,
        "delete_page",
        json!({ "page_id": PAGE, "base_version": "v0" }),
    )
    .await;
    assert_eq!(issue_codes(&del), vec!["versioning_unavailable"], "{del}");
    assert!(
        !call(&p, "expand", json!({ "page_id": PAGE })).await["page"].is_null(),
        "the refused delete must not have retracted the page"
    );

    // POSITIVE CONTROL: an UNGUARDED write on the same gateway still works.
    // Nobody asked for a guarantee, so nothing is withheld — this refusal is
    // about the guard, not about the gateway being read-only.
    let plain = call(
        &p,
        "update_page",
        json!({ "page_id": PAGE, "content": page("unguarded") }),
    )
    .await;
    assert_eq!(plain["ok"], true, "control: an unguarded write must still land: {plain}");
    assert!(
        call(&p, "expand", json!({ "page_id": PAGE })).await["body"]
            .as_str()
            .unwrap()
            .contains("unguarded"),
        "control: and it must really have landed"
    );

    p.shutdown().await;
}
