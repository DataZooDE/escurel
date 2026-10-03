//! The outbound policy of the remote (`openapi` / `mcp`) backends, over the real wire.
//!
//! A real gateway (`POST /mcp`, real OIDC, real DuckDB + `FsStore`) calls REAL upstream servers
//! bound to loopback sockets. The upstreams COUNT what reaches them, so a refusal is proven by the
//! upstream never having been called, not by an error string alone:
//!
//! - the strict default refuses a loopback upstream (the gateway must not be a way into the host);
//! - a redirect is refused, never followed (a public host cannot bounce the gateway inward);
//! - an oversize body and a slow upstream are bounded, not buffered or waited for;
//! - hostile values in a path template are percent-encoded, never spliced into the path.

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use axum::extract::{Path, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use duckdb::Connection;
use escurel_embed::{Embedder, ZeroEmbedder};
use escurel_index::{Indexer, Migrator};
use escurel_server::egress::EgressPolicy;
use escurel_storage::{FsStore, LaneStore};
use escurel_test_support::{AuthMode, ConfigOverrides, EscurelProcess, Opts, Role};
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::net::TcpListener;

const TENANT: &str = "acme";

const CUSTOMER_SKILL: &str = "---\n\
     kind: skill\n\
     id: customer\n\
     description: CRM customers, proxied live over REST.\n\
     backend:\n\
    \x20 kind: openapi\n\
    \x20 endpoint: crm_rest\n\
    \x20 read: { path: \"/customers/{id}\" }\n\
    \x20 write: { method: POST, path: \"/customers/{id}/orders/{order_id}\" }\n\
    \x20 project: { display_name: $.name, tier: $.account_tier }\n\
     ---\n\
     # customer\n";

async fn spawn_gateway(egress: EgressPolicy) -> (EscurelProcess, Vec<TempDir>) {
    let store_dir = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    let store: Arc<dyn LaneStore> = Arc::new(FsStore::new(store_dir.path().to_path_buf()));
    let embedder: Arc<dyn Embedder> = Arc::new(ZeroEmbedder::default());
    let conn = Connection::open(db_dir.path().join("escurel.duckdb")).unwrap();
    Migrator::up(&conn).unwrap();
    let indexer = Arc::new(Indexer::new(store, embedder, conn, TENANT).unwrap());
    indexer
        .update_page("markdown/skills/customer.md", CUSTOMER_SKILL)
        .await
        .unwrap();
    let process = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        config_overrides: ConfigOverrides {
            indexer: Some(indexer),
            egress: Some(egress),
            ..Default::default()
        },
        ..Default::default()
    })
    .await;
    (process, vec![store_dir, db_dir])
}

async fn call(p: &EscurelProcess, name: &str, args: Value) -> Value {
    let token = p.mint_token(TENANT, Role::Admin);
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

async fn serve(app: Router) -> (String, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://{addr}"), handle)
}

/// Register the endpoint, create the `acme` overlay, and `expand` it: the `backend_projection`.
async fn projection(p: &EscurelProcess, base_url: &str) -> Value {
    let reg = call(
        p,
        "register_endpoint",
        json!({ "name": "crm_rest", "kind": "openapi", "base_url": base_url }),
    )
    .await;
    assert!(reg.get("error").is_none(), "register: {reg}");
    let created = call(
        p,
        "create_remote_instance",
        json!({ "skill": "customer", "id": "acme" }),
    )
    .await;
    assert!(created.get("error").is_none(), "create: {created}");
    let page_id = created["result"]["structuredContent"]["page_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let body = call(p, "expand", json!({ "page_id": page_id })).await;
    body["result"]["structuredContent"]["backend_projection"].clone()
}

fn loopback_ok() -> EgressPolicy {
    EgressPolicy {
        allow_loopback: true,
        ..EgressPolicy::default()
    }
}

/// An upstream that counts every request it receives and answers a normal customer.
async fn counting_crm() -> (String, Arc<AtomicUsize>, tokio::task::JoinHandle<()>) {
    let hits = Arc::new(AtomicUsize::new(0));
    async fn customer(State(h): State<Arc<AtomicUsize>>, Path(_id): Path<String>) -> Json<Value> {
        h.fetch_add(1, Ordering::SeqCst);
        Json(json!({ "name": "Acme Corp", "account_tier": "gold" }))
    }
    let app = Router::new()
        .route("/customers/{id}", get(customer))
        .with_state(Arc::clone(&hits));
    let (base, handle) = serve(app).await;
    (base, hits, handle)
}

#[tokio::test]
async fn the_strict_default_refuses_a_loopback_upstream_and_never_calls_it() {
    let (base, hits, _srv) = counting_crm().await;
    let (process, _dirs) = spawn_gateway(EgressPolicy::default()).await;

    let proj = projection(&process, &base).await;

    let issue = proj["issue"].as_str().unwrap_or_default();
    assert!(
        issue.contains("egress policy"),
        "a loopback upstream must be refused by the strict default, got: {proj}"
    );
    assert_eq!(
        hits.load(Ordering::SeqCst),
        0,
        "the upstream must never have been called"
    );
    // And the refusal names no URL: the endpoint address is not for an agent to see.
    assert!(
        !issue.contains(&base),
        "the URL leaked into the issue: {issue}"
    );
    process.shutdown().await;
}

#[tokio::test]
async fn a_redirect_is_refused_and_never_followed() {
    // `inner` stands for the host a redirect would reach (the metadata service, in real life).
    let (inner_base, inner_hits, _inner) = counting_crm().await;
    let target = format!("{inner_base}/customers/acme");
    let app = Router::new().route(
        "/customers/{id}",
        get(move || {
            let t = target.clone();
            async move {
                let mut h = HeaderMap::new();
                h.insert("location", HeaderValue::from_str(&t).unwrap());
                (StatusCode::FOUND, h).into_response()
            }
        }),
    );
    let (front_base, _front) = serve(app).await;
    let (process, _dirs) = spawn_gateway(loopback_ok()).await;

    let proj = projection(&process, &front_base).await;

    let issue = proj["issue"].as_str().unwrap_or_default();
    assert!(
        issue.contains("redirect"),
        "a 3xx must be refused as a redirect, got: {proj}"
    );
    assert_eq!(
        inner_hits.load(Ordering::SeqCst),
        0,
        "the redirect target must never be called"
    );
    process.shutdown().await;
}

#[tokio::test]
async fn an_oversize_response_is_refused_not_buffered() {
    let app = Router::new().route(
        "/customers/{id}",
        get(|| async {
            let mut v = json!({ "name": "Acme" });
            v["padding"] = Value::String("x".repeat(200_000));
            Json(v)
        }),
    );
    let (base, _srv) = serve(app).await;
    let (process, _dirs) = spawn_gateway(EgressPolicy {
        max_response_bytes: 4096,
        ..loopback_ok()
    })
    .await;

    let proj = projection(&process, &base).await;

    let issue = proj["issue"].as_str().unwrap_or_default();
    assert!(
        issue.contains("larger than"),
        "an oversize body must be refused, got: {proj}"
    );
    process.shutdown().await;
}

#[tokio::test]
async fn a_slow_upstream_times_out_instead_of_hanging_the_read() {
    let app = Router::new().route(
        "/customers/{id}",
        get(|| async {
            tokio::time::sleep(Duration::from_secs(5)).await;
            Json(json!({ "name": "late" }))
        }),
    );
    let (base, _srv) = serve(app).await;
    let (process, _dirs) = spawn_gateway(EgressPolicy {
        timeout: Duration::from_millis(300),
        ..loopback_ok()
    })
    .await;

    let started = Instant::now();
    let proj = projection(&process, &base).await;

    let issue = proj["issue"].as_str().unwrap_or_default();
    assert!(
        issue.contains("did not answer"),
        "a slow upstream must time out, got: {proj}"
    );
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "the read hung for {:?}",
        started.elapsed()
    );
    process.shutdown().await;
}

#[tokio::test]
async fn a_hostile_value_in_a_path_template_is_encoded_never_spliced() {
    // The upstream records the RAW request paths it sees.
    let seen: Arc<std::sync::Mutex<Vec<String>>> = Arc::default();
    async fn any(
        State(seen): State<Arc<std::sync::Mutex<Vec<String>>>>,
        uri: axum::http::Uri,
    ) -> Response {
        seen.lock().unwrap().push(uri.path().to_owned());
        Json(json!({ "ok": true })).into_response()
    }
    let app = Router::new().fallback(any).with_state(Arc::clone(&seen));
    let (base, _srv) = serve(app).await;
    let (process, _dirs) = spawn_gateway(loopback_ok()).await;
    let reg = call(
        &process,
        "register_endpoint",
        json!({ "name": "crm_rest", "kind": "openapi", "base_url": base }),
    )
    .await;
    assert!(reg.get("error").is_none(), "register: {reg}");
    let created = call(
        &process,
        "create_remote_instance",
        json!({ "skill": "customer", "id": "acme" }),
    )
    .await;
    assert!(created.get("error").is_none(), "create: {created}");

    let _ = call(
        &process,
        "write_instance",
        json!({ "ref": "customer::acme",
                "payload": { "order_id": "../../admin/users?x=1#frag" } }),
    )
    .await;

    for path in seen.lock().unwrap().iter() {
        assert!(
            !path.contains("..") && !path.contains("/admin/"),
            "a hostile template value reached the upstream path unencoded: {path}"
        );
        assert!(
            path.starts_with("/customers/acme/orders/"),
            "the path must stay under its template: {path}"
        );
    }
    process.shutdown().await;
}

// --- secrets: references, never material ------------------------------------------------------

/// An upstream that REQUIRES `Authorization: Bearer <token>`, records the header it saw, and on a
/// wrong token answers 401 with a body that ECHOES the credential it was sent (as careless
/// upstreams do) — the gateway must not pass that text on.
async fn authed_crm(token: &'static str) -> (String, Arc<std::sync::Mutex<Vec<String>>>) {
    let seen: Arc<std::sync::Mutex<Vec<String>>> = Arc::default();
    let app = Router::new()
        .route(
            "/customers/{id}",
            get(
                move |State(seen): State<Arc<std::sync::Mutex<Vec<String>>>>,
                      headers: HeaderMap| async move {
                    let got = headers
                        .get("authorization")
                        .and_then(|v| v.to_str().ok())
                        .unwrap_or("")
                        .to_owned();
                    seen.lock().unwrap().push(got.clone());
                    if got == format!("Bearer {token}") {
                        Json(json!({ "name": "Acme Corp", "account_tier": "gold" })).into_response()
                    } else {
                        (StatusCode::UNAUTHORIZED, format!("bad credential: {got}")).into_response()
                    }
                },
            ),
        )
        .with_state(Arc::clone(&seen));
    let (base, _h) = serve(app).await;
    (base, seen)
}

async fn register_bearer(p: &EscurelProcess, base: &str, secret_field: (&str, &str)) -> Value {
    call(
        p,
        "register_endpoint",
        json!({ "name": "crm_rest", "kind": "openapi", "base_url": base,
                "auth": "bearer", secret_field.0: secret_field.1 }),
    )
    .await
}

async fn expand_acme(p: &EscurelProcess) -> Value {
    let created = call(
        p,
        "create_remote_instance",
        json!({ "skill": "customer", "id": "acme" }),
    )
    .await;
    let page_id = created["result"]["structuredContent"]["page_id"]
        .as_str()
        .expect("page_id")
        .to_owned();
    let body = call(p, "expand", json!({ "page_id": page_id })).await;
    body["result"]["structuredContent"]["backend_projection"].clone()
}

#[tokio::test]
async fn a_secret_reference_is_resolved_at_call_time_and_never_stored_or_echoed() {
    let token = "tok-7f3a9c-DO-NOT-LEAK";
    let secret_dir = TempDir::new().unwrap();
    let secret_file = secret_dir.path().join("crm-token");
    std::fs::write(&secret_file, format!("{token}\n")).unwrap();
    let secret_ref = format!("file:{}", secret_file.display());
    let (base, seen) = authed_crm(token).await;
    let (process, _dirs) = spawn_gateway(loopback_ok()).await;

    let reg = register_bearer(&process, &base, ("secret_ref", secret_ref.as_str())).await;
    assert!(reg.get("error").is_none(), "register: {reg}");
    assert!(
        !reg.to_string().contains(token),
        "the token leaked into register: {reg}"
    );

    let proj = expand_acme(&process).await;
    assert_eq!(
        proj["fields"]["display_name"], "Acme Corp",
        "authenticated read: {proj}"
    );
    assert_eq!(
        seen.lock().unwrap().last().map(String::as_str),
        Some(&*format!("Bearer {token}")),
        "the upstream must have received the resolved token"
    );

    let list = call(&process, "list_endpoints", json!({})).await;
    assert!(
        !list.to_string().contains(token),
        "the token leaked into list: {list}"
    );
    assert_eq!(
        list["result"]["structuredContent"]["endpoints"][0]["secret_kind"], "ref",
        "a reference is reported as such: {list}"
    );
    process.shutdown().await;
}

#[tokio::test]
async fn an_unset_reference_degrades_naming_the_reference_not_a_value() {
    let (base, seen) = authed_crm("whatever").await;
    let (process, _dirs) = spawn_gateway(loopback_ok()).await;
    let reg = register_bearer(
        &process,
        &base,
        ("secret_ref", "file:/nonexistent/escurel/never-set"),
    )
    .await;
    assert!(reg.get("error").is_none(), "register: {reg}");

    let proj = expand_acme(&process).await;

    let issue = proj["issue"].as_str().unwrap_or_default();
    assert!(
        issue.contains("file:/nonexistent/escurel/never-set") && issue.contains("not available"),
        "the issue must name the missing reference: {proj}"
    );
    assert!(
        seen.lock().unwrap().is_empty(),
        "an unauthenticated call must not be made"
    );
    process.shutdown().await;
}

#[tokio::test]
async fn an_inline_secret_still_works_but_is_flagged_deprecated_and_never_listed() {
    let token = "inline-tok-91b2-DO-NOT-LEAK";
    let (base, _seen) = authed_crm(token).await;
    let (process, _dirs) = spawn_gateway(loopback_ok()).await;

    let reg = register_bearer(&process, &base, ("secret", token)).await;
    assert!(reg.get("error").is_none(), "register: {reg}");
    assert!(
        reg["result"]["structuredContent"]["warning"]
            .as_str()
            .is_some_and(|w| w.contains("secret_ref")),
        "an inline secret must be flagged and point at secret_ref: {reg}"
    );
    assert!(!reg.to_string().contains(token), "{reg}");

    let list = call(&process, "list_endpoints", json!({})).await;
    assert_eq!(
        list["result"]["structuredContent"]["endpoints"][0]["secret_kind"],
        "inline"
    );
    assert!(
        !list.to_string().contains(token),
        "the token leaked into list: {list}"
    );
    let proj = expand_acme(&process).await;
    assert_eq!(proj["fields"]["display_name"], "Acme Corp", "{proj}");
    process.shutdown().await;
}

#[tokio::test]
async fn an_upstream_that_echoes_the_credential_in_its_error_is_not_repeated() {
    // The registered token is WRONG for this upstream, so it answers 401 echoing what it got.
    let secret = "wrong-tok-55d1-DO-NOT-LEAK";
    let secret_dir = TempDir::new().unwrap();
    let secret_file = secret_dir.path().join("wrong-token");
    std::fs::write(&secret_file, secret).unwrap();
    let secret_ref = format!("file:{}", secret_file.display());
    let (base, _seen) = authed_crm("the-real-token").await;
    let (process, _dirs) = spawn_gateway(loopback_ok()).await;
    let reg = register_bearer(&process, &base, ("secret_ref", secret_ref.as_str())).await;
    assert!(reg.get("error").is_none(), "register: {reg}");

    let proj = expand_acme(&process).await;

    assert!(
        proj["issue"].as_str().is_some_and(|i| i.contains("401")),
        "{proj}"
    );
    assert!(
        !proj.to_string().contains(secret),
        "the upstream's echo leaked: {proj}"
    );
    process.shutdown().await;
}
