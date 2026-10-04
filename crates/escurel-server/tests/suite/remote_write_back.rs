//! Write-back to a REAL REST upstream (stage 4c): a human-gated change to a row's writable column.
//!
//! The model: an agent (or a person) proposes the change as a DRAFT carrying a `write_back` intent
//! (`patch` + the `base_etag` it was based on); nothing reaches the upstream until a human PROMOTES
//! the draft. The promote hook then (1) re-reads the row and refuses on a changed etag, (2) writes an
//! audit event BEFORE calling out, (3) applies with an idempotency key and bounded retries, (4)
//! records the outcome as a durable event, and only then (5) commits the notes and closes the draft.
//! The outcome event is the witness that makes a re-promote safe: the upstream is never called twice.
//!
//! The upstream is a real `axum` server that enforces `If-Match`, applies each `Idempotency-Key` once,
//! can be told to fail or reject, and counts every request, so every claim is about what crossed the
//! wire.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use axum::extract::{Path, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use escurel_test_support::{EgressPolicy, Role};
use serde_json::{Value, json};

use super::remote_support::{admin, call_as, serve, spawn_gateway};

const CUSTOMER_SKILL: &str = "---\n\
     kind: skill\n\
     id: customer\n\
     description: CRM customers; `tier` may be changed through a human-gated write-back.\n\
     backend:\n\
    \x20 kind: openapi\n\
    \x20 endpoint: crm_rest\n\
    \x20 instances: rows\n\
    \x20 key: $.id\n\
    \x20 linked: true\n\
    \x20 writable_columns: [tier]\n\
    \x20 list: { path: /customers, items: $.data }\n\
    \x20 read: { path: \"/customers/{id}\" }\n\
    \x20 write: { method: PATCH, path: \"/customers/{id}\" }\n\
    \x20 project: { display_name: $.name, tier: $.account_tier }\n\
     ---\n\
     # customer\n";

#[derive(Default)]
struct Crm {
    rows: Mutex<BTreeMap<String, Value>>,
    version: AtomicUsize,
    /// Every PATCH request received: (Idempotency-Key, If-Match, body).
    patches: Mutex<Vec<(String, String, Value)>>,
    /// Idempotency keys already applied (the apply happens once per key).
    applied_keys: Mutex<Vec<String>>,
    /// Answer 503 to the next N PATCHes.
    fail_next: AtomicUsize,
    /// Answer 400 to every PATCH.
    reject: std::sync::atomic::AtomicBool,
    /// Answer 503 to every PATCH.
    down: std::sync::atomic::AtomicBool,
    /// Answer 503 to every GET (the portal is unreachable before anything is sent).
    read_down: std::sync::atomic::AtomicBool,
    /// Hold every PATCH this long before answering (a slow portal).
    patch_delay_ms: AtomicUsize,
}

impl Crm {
    fn etag(&self) -> String {
        format!("\"v{}\"", self.version.load(Ordering::SeqCst))
    }
    fn tier(&self, id: &str) -> String {
        self.rows.lock().unwrap()[id]["account_tier"]
            .as_str()
            .unwrap()
            .to_owned()
    }
    fn bump(&self) {
        self.version.fetch_add(1, Ordering::SeqCst);
    }
}

async fn get_one(State(c): State<Arc<Crm>>, Path(id): Path<String>) -> Response {
    if c.read_down.load(Ordering::SeqCst) {
        return (StatusCode::SERVICE_UNAVAILABLE, "down: do-not-repeat-this").into_response();
    }
    let rows = c.rows.lock().unwrap();
    match rows.get(&id) {
        Some(v) => {
            let mut h = HeaderMap::new();
            h.insert("etag", HeaderValue::from_str(&c.etag()).unwrap());
            (h, Json(v.clone())).into_response()
        }
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn list(State(c): State<Arc<Crm>>) -> Json<Value> {
    let rows: Vec<Value> = c.rows.lock().unwrap().values().cloned().collect();
    Json(json!({ "data": rows }))
}

async fn patch(
    State(c): State<Arc<Crm>>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    let hv = |n: &str| {
        headers
            .get(n)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_owned()
    };
    let key = hv("idempotency-key");
    let if_match = hv("if-match");
    let delay = c.patch_delay_ms.load(Ordering::SeqCst);
    if delay > 0 {
        tokio::time::sleep(std::time::Duration::from_millis(delay as u64)).await;
    }
    c.patches
        .lock()
        .unwrap()
        .push((key.clone(), if_match.clone(), body.clone()));
    if c.down.load(Ordering::SeqCst) {
        return (StatusCode::SERVICE_UNAVAILABLE, "down: do-not-repeat").into_response();
    }
    if c.fail_next.load(Ordering::SeqCst) > 0 {
        c.fail_next.fetch_sub(1, Ordering::SeqCst);
        return (StatusCode::SERVICE_UNAVAILABLE, "try later").into_response();
    }
    if c.reject.load(Ordering::SeqCst) {
        return (StatusCode::BAD_REQUEST, "no such tier").into_response();
    }
    if key.is_empty() {
        return (StatusCode::BAD_REQUEST, "Idempotency-Key required").into_response();
    }
    // A key already applied answers success WITHOUT applying again.
    if c.applied_keys.lock().unwrap().contains(&key) {
        return Json(json!({ "ok": true, "replayed": true })).into_response();
    }
    if !if_match.is_empty() && if_match != c.etag() {
        return StatusCode::PRECONDITION_FAILED.into_response();
    }
    let mut rows = c.rows.lock().unwrap();
    let Some(row) = rows.get_mut(&id) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if let (Some(o), Some(p)) = (row.as_object_mut(), body.as_object()) {
        for (k, v) in p {
            o.insert(k.clone(), v.clone());
        }
    }
    c.applied_keys.lock().unwrap().push(key);
    drop(rows);
    c.bump();
    Json(json!({ "ok": true })).into_response()
}

fn crm() -> Arc<Crm> {
    let c = Arc::new(Crm::default());
    c.rows.lock().unwrap().insert(
        "c-0001".to_owned(),
        json!({ "id": "c-0001", "name": "Acme Corp", "account_tier": "silver" }),
    );
    c
}

async fn gateway_over(
    c: &Arc<Crm>,
    retry_ms: u64,
) -> (escurel_test_support::EscurelProcess, Vec<tempfile::TempDir>) {
    let app = Router::new()
        .route("/customers", get(list))
        .route("/customers/{id}", get(get_one).patch(patch))
        .with_state(Arc::clone(c));
    let (base, _h) = serve(app).await;
    let (p, dirs) = spawn_gateway(
        &[("customer", CUSTOMER_SKILL)],
        EgressPolicy {
            allow_loopback: true,
            write_retry_backoff: std::time::Duration::from_millis(retry_ms),
            ..EgressPolicy::default()
        },
    )
    .await;
    admin(
        &p,
        "register_endpoint",
        json!({ "name": "crm_rest", "kind": "openapi", "base_url": base }),
    )
    .await;
    (p, dirs)
}

const PAGE: &str = "markdown/instances/customer/c-0001.md";

async fn etag_of_row(p: &escurel_test_support::EscurelProcess) -> String {
    let page = admin(p, "expand", json!({ "page_id": PAGE })).await;
    page["backend_projection"]["etag"]
        .as_str()
        .expect("etag in the projection")
        .to_owned()
}

fn intent_content(patch: &str, base_etag: &str, notes: &str) -> String {
    format!(
        "---\nkind: instance\nid: c-0001\nskill: customer\nwrite_back:\n  patch: {{ {patch} }}\n  base_etag: \"{base_etag}\"\n---\n{notes}\n"
    )
}

async fn draft(p: &escurel_test_support::EscurelProcess, content: &str) -> Value {
    call_as(
        p,
        Role::Admin,
        "create_draft",
        json!({ "target_page_id": PAGE, "content": content }),
    )
    .await
}

fn draft_id(v: &Value) -> String {
    v["result"]["structuredContent"]["draft"]["draft_id"]
        .as_str()
        .unwrap_or_else(|| panic!("no draft id: {v}"))
        .to_owned()
}

async fn promote(p: &escurel_test_support::EscurelProcess, id: &str) -> Value {
    call_as(p, Role::Admin, "promote_draft", json!({ "draft_id": id })).await["result"]["structuredContent"].clone()
}

fn issue_codes(v: &Value) -> Vec<String> {
    v["issues"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|i| i["code"].as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

async fn events(p: &escurel_test_support::EscurelProcess) -> Vec<Value> {
    let v = admin(
        p,
        "list_events",
        json!({ "label_skill": "escurel:write-back", "include_system": true }),
    )
    .await;
    v["events"].as_array().cloned().unwrap_or_default()
}

#[tokio::test]
async fn a_promoted_write_back_is_applied_once_with_an_idempotency_key_and_an_etag_precondition() {
    let c = crm();
    let (p, _d) = gateway_over(&c, 5).await;
    let etag = etag_of_row(&p).await;
    let d = draft(
        &p,
        &intent_content("tier: gold", &etag, "Upgraded after the renewal call."),
    )
    .await;
    let id = draft_id(&d);
    // The page tells a client which columns may be proposed for write-back.
    let proj = admin(&p, "expand", json!({ "page_id": PAGE })).await;
    assert_eq!(
        proj["backend_projection"]["writable_columns"],
        json!(["tier"]),
        "the projection names the writable columns: {proj}"
    );
    assert_eq!(
        c.patches.lock().unwrap().len(),
        0,
        "creating a draft must not touch the upstream"
    );

    let done = promote(&p, &id).await;

    assert_eq!(done["ok"], true, "{done}");
    assert_eq!(c.tier("c-0001"), "gold", "the upstream applied the change");
    let patches = c.patches.lock().unwrap().clone();
    assert_eq!(patches.len(), 1, "exactly one PATCH");
    assert_eq!(patches[0].0, id, "the Idempotency-Key is the draft id");
    assert_eq!(
        patches[0].1, "\"v0\"",
        "If-Match carries the upstream's own ETag"
    );
    assert_eq!(
        patches[0].2,
        json!({ "account_tier": "gold" }),
        "the UPSTREAM's field name, nothing else"
    );
    // The notes were committed WITHOUT the intent, and the row reads back from the upstream.
    let page = admin(&p, "expand", json!({ "page_id": PAGE })).await;
    assert!(
        page["body"]
            .as_str()
            .unwrap()
            .contains("Upgraded after the renewal call"),
        "{page}"
    );
    assert!(
        page["frontmatter"].get("write_back").is_none(),
        "the intent is not stored: {page}"
    );
    assert_eq!(page["frontmatter"]["tier"], "gold", "{page}");
    // The audit trail: applying BEFORE the call, applied after, no values in it.
    let ev = events(&p).await;
    let ids: Vec<&str> = ev.iter().filter_map(|e| e["event_id"].as_str()).collect();
    assert!(
        ids.iter().any(|i| i.ends_with(":applying")) && ids.iter().any(|i| i.ends_with(":applied")),
        "{ids:?}"
    );
    assert!(
        !ev.iter().any(|e| e.to_string().contains("gold")),
        "values must not be audited: {ev:?}"
    );
    p.shutdown().await;
}

#[tokio::test]
async fn a_row_that_moved_since_the_draft_conflicts_and_the_upstream_is_never_called() {
    let c = crm();
    let (p, _d) = gateway_over(&c, 5).await;
    let etag = etag_of_row(&p).await;
    let id = draft_id(&draft(&p, &intent_content("tier: gold", &etag, "n")).await);
    // Someone else changes the row after the draft was made.
    c.rows.lock().unwrap().get_mut("c-0001").unwrap()["account_tier"] = json!("platinum");

    let done = promote(&p, &id).await;

    assert_eq!(done["ok"], false, "{done}");
    assert!(
        issue_codes(&done).contains(&"write_back_conflict".to_owned()),
        "{done}"
    );
    assert_eq!(
        c.patches.lock().unwrap().len(),
        0,
        "a conflict must not call the upstream"
    );
    assert_eq!(c.tier("c-0001"), "platinum");
    let still = admin(&p, "list_drafts", json!({})).await;
    assert!(
        still["drafts"]
            .as_array()
            .unwrap()
            .iter()
            .any(|x| x["draft_id"] == id.as_str() && x["status"] == "open"),
        "the draft stays open: {still}"
    );
    p.shutdown().await;
}

#[tokio::test]
async fn only_writable_columns_may_be_proposed_and_an_intent_never_lands_through_update_page() {
    let c = crm();
    let (p, _d) = gateway_over(&c, 5).await;
    let etag = etag_of_row(&p).await;

    let bad = draft(&p, &intent_content("display_name: Renamed", &etag, "n")).await;
    let text = bad.to_string();
    assert!(
        text.contains("backend_read_only_field"),
        "a non-writable column is refused: {bad}"
    );

    let direct = call_as(
        &p,
        Role::Admin,
        "update_page",
        json!({ "page_id": PAGE,
        "content": intent_content("tier: gold", &etag, "n") }),
    )
    .await;
    assert!(
        direct.to_string().contains("write_back_requires_draft"),
        "an intent needs a human gate: {direct}"
    );
    assert_eq!(c.patches.lock().unwrap().len(), 0);
    p.shutdown().await;
}

#[tokio::test]
async fn a_transient_failure_is_retried_with_the_same_key_and_applied_once() {
    let c = crm();
    c.fail_next.store(2, Ordering::SeqCst);
    let (p, _d) = gateway_over(&c, 5).await;
    let etag = etag_of_row(&p).await;
    let id = draft_id(&draft(&p, &intent_content("tier: gold", &etag, "n")).await);

    let done = promote(&p, &id).await;

    assert_eq!(done["ok"], true, "{done}");
    let patches = c.patches.lock().unwrap().clone();
    assert_eq!(patches.len(), 3, "two 503s then success");
    assert!(
        patches.iter().all(|p| p.0 == id),
        "every attempt carries the SAME idempotency key: {patches:?}"
    );
    assert_eq!(
        c.applied_keys.lock().unwrap().len(),
        1,
        "applied exactly once"
    );
    assert_eq!(c.tier("c-0001"), "gold");
    p.shutdown().await;
}

#[tokio::test]
async fn a_rejection_is_not_retried_and_leaves_the_draft_open() {
    let c = crm();
    c.reject.store(true, Ordering::SeqCst);
    let (p, _d) = gateway_over(&c, 5).await;
    let etag = etag_of_row(&p).await;
    let id = draft_id(&draft(&p, &intent_content("tier: gold", &etag, "n")).await);

    let done = promote(&p, &id).await;

    assert_eq!(done["ok"], false, "{done}");
    assert!(
        issue_codes(&done).contains(&"write_back_rejected".to_owned()),
        "{done}"
    );
    assert!(
        !done.to_string().contains("no such tier"),
        "the upstream's body is not repeated: {done}"
    );
    assert_eq!(c.patches.lock().unwrap().len(), 1, "a 4xx is never retried");
    p.shutdown().await;
}

#[tokio::test]
async fn an_outage_dead_letters_after_bounded_retries_and_a_later_promote_succeeds() {
    let c = crm();
    c.down.store(true, Ordering::SeqCst);
    let (p, _d) = gateway_over(&c, 5).await;
    let etag = etag_of_row(&p).await;
    let id = draft_id(&draft(&p, &intent_content("tier: gold", &etag, "n")).await);

    let first = promote(&p, &id).await;

    assert_eq!(first["ok"], false, "{first}");
    assert!(
        issue_codes(&first).contains(&"write_back_failed".to_owned()),
        "{first}"
    );
    assert!(!first.to_string().contains("do-not-repeat"), "{first}");
    assert_eq!(
        c.patches.lock().unwrap().len(),
        3,
        "bounded: three attempts, then dead-letter"
    );
    let ev = events(&p).await;
    assert!(
        ev.iter().any(|e| e["event_id"]
            .as_str()
            .is_some_and(|i| i.ends_with(":failed"))),
        "a dead-letter event is recorded: {ev:?}"
    );
    assert_eq!(
        c.tier("c-0001"),
        "silver",
        "nothing applied locally or upstream"
    );

    // The upstream recovers; promoting again works.
    c.down.store(false, Ordering::SeqCst);
    let again = promote(&p, &id).await;
    assert_eq!(again["ok"], true, "{again}");
    assert_eq!(c.tier("c-0001"), "gold");
    p.shutdown().await;
}

#[tokio::test]
async fn a_local_failure_after_the_upstream_applied_never_calls_the_upstream_twice() {
    // The upstream applies; the LOCAL commit then fails (the notes page changed under the draft).
    // The outcome event is the durable witness: promoting again must skip the upstream.
    let c = crm();
    let (p, _d) = gateway_over(&c, 5).await;
    let etag = etag_of_row(&p).await;
    let content = intent_content("tier: gold", &etag, "draft notes");
    // `base_sha256: ""` = "there is no notes page yet": the draft's local commit is CAS-guarded on it.
    let id = draft_id(
        &call_as(
            &p,
            Role::Admin,
            "create_draft",
            json!({ "target_page_id": PAGE, "content": content, "base_sha256": "" }),
        )
        .await,
    );
    // Make the notes page appear AFTER the draft, so the draft's "no page yet" base conflicts.
    let _ = call_as(&p, Role::Admin, "update_page", json!({ "page_id": PAGE,
        "content": "---\nkind: instance\nid: c-0001\nskill: customer\n---\nsomeone else wrote notes first\n" })).await;

    let first = promote(&p, &id).await;

    assert_eq!(first["ok"], false, "the local commit conflicts: {first}");
    assert_eq!(
        c.applied_keys.lock().unwrap().len(),
        1,
        "the upstream DID apply"
    );
    let before = c.patches.lock().unwrap().len();

    let second = promote(&p, &id).await;

    assert_eq!(second["ok"], false, "still conflicting locally: {second}");
    assert_eq!(
        c.patches.lock().unwrap().len(),
        before,
        "the upstream is NOT called again: the outcome event is the witness"
    );
    p.shutdown().await;
}

#[tokio::test]
async fn an_outage_before_anything_is_sent_is_still_audited_and_leaves_the_draft_open() {
    // The portal cannot even be READ to check the row (the etag precondition), so nothing is sent. The
    // refusal must still leave a trace: a promoted change that fails silently has no audit trail and a
    // page that cannot say what happened.
    let c = crm();
    let (p, _d) = gateway_over(&c, 5).await;
    let etag = etag_of_row(&p).await;
    let id = draft_id(&draft(&p, &intent_content("tier: gold", &etag, "n")).await);
    c.read_down.store(true, Ordering::SeqCst);

    let first = promote(&p, &id).await;

    assert_eq!(first["ok"], false, "{first}");
    assert!(
        issue_codes(&first).contains(&"write_back_failed".to_owned()),
        "{first}"
    );
    let text = first.to_string();
    assert!(
        !text.contains("do-not-repeat") && !text.contains("127.0.0.1"),
        "{first}"
    );
    assert!(
        text.contains("could not be reached"),
        "worded for a person: {first}"
    );
    assert!(c.patches.lock().unwrap().is_empty(), "nothing was sent");
    let ev = events(&p).await;
    let failed = ev
        .iter()
        .find(|e| {
            e["event_id"]
                .as_str()
                .is_some_and(|i| i.ends_with(":failed"))
        })
        .unwrap_or_else(|| panic!("a failed event is recorded: {ev:?}"));
    let body: Value = serde_json::from_str(failed["body"].as_str().unwrap()).unwrap();
    assert_eq!(body["outcome"], "failed", "{body}");
    assert_eq!(body["attempts"], 0, "{body}");
    assert_eq!(body["draft_id"], id, "{body}");
    assert_eq!(c.tier("c-0001"), "silver");

    // The portal comes back; promoting again applies it.
    c.read_down.store(false, Ordering::SeqCst);
    let again = promote(&p, &id).await;
    assert_eq!(again["ok"], true, "{again}");
    assert_eq!(c.tier("c-0001"), "gold");
    p.shutdown().await;
}

// ── Crew review (robustness/security), reproduced through the real gateway ────────────────────

/// `tools/call` as `role` with a client-side deadline: when it fires the request is DROPPED, which
/// is what a disconnecting browser or a proxy timeout does to the handler future.
async fn call_with_deadline(
    p: &escurel_test_support::EscurelProcess,
    role: Role,
    name: &str,
    args: Value,
    deadline: std::time::Duration,
) {
    let token = p.mint_token(super::remote_support::TENANT, role);
    let _ = reqwest::Client::builder()
        .timeout(deadline)
        .build()
        .unwrap()
        .post(p.mcp_url())
        .header("authorization", format!("Bearer {token}"))
        .json(&json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                       "params": { "name": name, "arguments": args } }))
        .send()
        .await;
}

#[tokio::test]
async fn an_agent_cannot_forge_the_applied_witness_to_make_a_promote_skip_the_upstream() {
    let c = crm();
    let (p, _d) = gateway_over(&c, 5).await;
    let etag = etag_of_row(&p).await;
    let id = draft_id(&draft(&p, &intent_content("tier: gold", &etag, "n")).await);

    // Any agent can see the draft and file an event: it must not be able to file the WITNESS.
    let forged = call_as(
        &p,
        Role::Agent,
        "capture_event",
        json!({
            "event_id": format!("write-back:{id}:applied"),
            "label_skill": "note",
            "source": "agent",
            "mime": "text/plain",
            "title": "looks applied",
            "body": "trust me",
        }),
    )
    .await;
    let refused = forged.get("error").is_some()
        || forged["result"]["isError"] == json!(true)
        || forged["result"]["structuredContent"]["ok"] == json!(false);
    assert!(
        refused,
        "a caller-supplied write-back: event id must be refused: {forged}"
    );

    let done = promote(&p, &id).await;
    assert_eq!(done["ok"], true, "{done}");
    assert_eq!(
        c.patches.lock().unwrap().len(),
        1,
        "the promote reached the upstream: nothing a caller filed could stand in for the witness"
    );
    assert_eq!(c.tier("c-0001"), "gold");
    p.shutdown().await;
}

#[tokio::test]
async fn a_promoter_cannot_write_a_column_the_skill_does_not_declare_writable() {
    let c = crm();
    let (p, _d) = gateway_over(&c, 5).await;
    let etag = etag_of_row(&p).await;
    let id = draft_id(&draft(&p, &intent_content("tier: gold", &etag, "n")).await);

    // The approver's "correction" swaps the patch for a PROJECTED but not writable column.
    let corrected = intent_content("display_name: Evil Corp", &etag, "n");
    let done = call_as(
        &p,
        Role::Admin,
        "promote_draft",
        json!({ "draft_id": id, "content": corrected }),
    )
    .await["result"]["structuredContent"]
        .clone();

    assert_eq!(done["ok"], false, "{done}");
    assert!(
        issue_codes(&done).contains(&"backend_read_only_field".to_owned()),
        "{done}"
    );
    assert_eq!(
        c.patches.lock().unwrap().len(),
        0,
        "writable_columns is enforced at promote time too: nothing reached the upstream"
    );
    p.shutdown().await;
}

#[tokio::test]
async fn two_promotes_racing_on_one_draft_reach_the_upstream_once() {
    let c = crm();
    c.patch_delay_ms.store(400, Ordering::SeqCst);
    let (p, _d) = gateway_over(&c, 5).await;
    let etag = etag_of_row(&p).await;
    let id = draft_id(&draft(&p, &intent_content("tier: gold", &etag, "n")).await);

    let (a, b) = tokio::join!(promote(&p, &id), promote(&p, &id));

    // The second promote waits for the first, finds its witness and only completes the local half
    // (or is told the draft was already decided): either way it never reaches the upstream.
    assert_eq!(
        c.patches.lock().unwrap().len(),
        1,
        "the upstream is called once however many promotes race: {a} / {b}"
    );
    p.shutdown().await;
}

#[tokio::test]
async fn a_dropped_promote_request_still_applies_and_records_its_witness() {
    let c = crm();
    c.patch_delay_ms.store(700, Ordering::SeqCst);
    let (p, _d) = gateway_over(&c, 5).await;
    let etag = etag_of_row(&p).await;
    let id = draft_id(&draft(&p, &intent_content("tier: gold", &etag, "n")).await);

    // The client gives up long before the slow upstream answers.
    call_with_deadline(
        &p,
        Role::Admin,
        "promote_draft",
        json!({ "draft_id": id }),
        std::time::Duration::from_millis(150),
    )
    .await;

    // The apply must not depend on the request that started it: the upstream applied, so the
    // witness has to exist, and a retry must not call the upstream again.
    let mut witnessed = false;
    for _ in 0..40 {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        if events(&p).await.iter().any(|e| {
            e["event_id"]
                .as_str()
                .is_some_and(|i| i.ends_with(":applied"))
        }) {
            witnessed = true;
            break;
        }
    }
    assert!(
        witnessed,
        "the applied witness was lost with the dropped request"
    );
    assert_eq!(c.tier("c-0001"), "gold");
    let again = promote(&p, &id).await;
    assert_eq!(
        again["ok"], true,
        "the retry completes the local half: {again}"
    );
    assert_eq!(
        c.patches.lock().unwrap().len(),
        1,
        "and does not call the upstream a second time"
    );
    p.shutdown().await;
}

#[tokio::test]
async fn a_row_that_already_holds_the_change_counts_as_applied_not_as_a_conflict() {
    // The upstream applied the change but its witness was lost (a crash between the call and the
    // audit write): the row now carries OUR change, so its etag no longer matches the draft's base.
    let c = crm();
    let (p, _d) = gateway_over(&c, 5).await;
    let etag = etag_of_row(&p).await;
    let id = draft_id(&draft(&p, &intent_content("tier: gold", &etag, "n")).await);
    c.rows.lock().unwrap().get_mut("c-0001").unwrap()["account_tier"] = json!("gold");
    c.bump();

    let done = promote(&p, &id).await;

    assert_eq!(
        done["ok"], true,
        "a row already equal to the patch is applied, not stuck in conflict forever: {done}"
    );
    assert_eq!(
        c.patches.lock().unwrap().len(),
        0,
        "and the upstream is not asked to do it again"
    );
    p.shutdown().await;
}

// ---- the human gate is a rule, not a convention ----------------------------------------------------

async fn call_with_token(
    p: &escurel_test_support::EscurelProcess,
    token: &str,
    name: &str,
    args: Value,
) -> Value {
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

#[tokio::test]
async fn an_agent_run_token_cannot_promote_a_write_back_draft_it_can_only_propose_one() {
    // Anyone who could SEE a draft could promote it, so a prompt-injected agent could propose a
    // write-back and approve it itself: the "human gate" was only a convention.
    let c = crm();
    let (p, _d) = gateway_over(&c, 5).await;
    let etag = etag_of_row(&p).await;
    let minted = admin(
        &p,
        "mint_agent_token",
        json!({ "skill": "customer", "target_page_id": PAGE }),
    )
    .await["token"]
        .as_str()
        .expect("a minted run token")
        .to_owned();

    let created = call_with_token(
        &p,
        &minted,
        "create_draft",
        json!({ "target_page_id": PAGE, "content": intent_content("tier: gold", &etag, "n") }),
    )
    .await;
    let id = draft_id(&created);

    let own = call_with_token(&p, &minted, "promote_draft", json!({ "draft_id": id })).await;
    let out = own["result"]["structuredContent"].clone();
    assert_eq!(
        out["ok"], false,
        "the agent must not approve its own write-back: {own}"
    );
    assert!(
        issue_codes(&out).contains(&"promote_requires_human".to_owned()),
        "{own}"
    );
    assert!(
        c.patches.lock().unwrap().is_empty(),
        "nothing may reach the upstream on an agent's say-so"
    );

    // A person (a token that is not a minted run token) still can, and it goes through.
    let done = promote(&p, &id).await;
    assert_eq!(done["ok"], true, "{done}");
    assert_eq!(c.patches.lock().unwrap().len(), 1);
    p.shutdown().await;
}

// ── Operators can SEE write-back outcomes: `escurel_write_back_total{outcome}` ────────────────

#[tokio::test]
async fn an_applied_write_back_is_counted() {
    let c = crm();
    let (p, _d) = gateway_over(&c, 5).await;
    let etag = etag_of_row(&p).await;
    let id = draft_id(&draft(&p, &intent_content("tier: gold", &etag, "n")).await);
    assert_eq!(promote(&p, &id).await["ok"], true);
    assert_eq!(
        super::remote_support::metric(&p, r#"escurel_write_back_total{outcome="applied"}"#).await,
        Some(1.0)
    );
    p.shutdown().await;
}

#[tokio::test]
async fn a_conflicting_write_back_is_counted() {
    let c = crm();
    let (p, _d) = gateway_over(&c, 5).await;
    let etag = etag_of_row(&p).await;
    let id = draft_id(&draft(&p, &intent_content("tier: gold", &etag, "n")).await);
    c.rows.lock().unwrap().get_mut("c-0001").unwrap()["account_tier"] = json!("platinum");
    assert_eq!(promote(&p, &id).await["ok"], false);
    assert_eq!(
        super::remote_support::metric(&p, r#"escurel_write_back_total{outcome="conflict"}"#).await,
        Some(1.0)
    );
    p.shutdown().await;
}

#[tokio::test]
async fn a_dead_lettered_write_back_is_counted() {
    let c = crm();
    let (p, _d) = gateway_over(&c, 5).await;
    let etag = etag_of_row(&p).await;
    let id = draft_id(&draft(&p, &intent_content("tier: gold", &etag, "n")).await);
    c.down.store(true, Ordering::SeqCst);
    assert_eq!(promote(&p, &id).await["ok"], false);
    assert_eq!(
        super::remote_support::metric(&p, r#"escurel_write_back_total{outcome="dead_letter"}"#)
            .await,
        Some(1.0)
    );
    p.shutdown().await;
}
