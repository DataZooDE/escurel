//! No-mock integration tests for the explore BFF.
//!
//! The auth assertion (test 1) uses the REAL `escurel_auth::OidcVerifier`
//! as the boundary — the BFF's minted token is verified by the same code
//! escurel runs in production, with the JWKS fetched from the BFF's own
//! `/jwks.json`. No mocks at that boundary.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::extract::State;
use axum::http::HeaderMap;
use axum::routing::post;
use tokio::net::TcpListener;

use escurel_auth::{OidcConfig, OidcVerifier, Role};
use escurel_explore_bff::{Config, serve_on};

/// Bind a loopback listener and return it with its address. Lets a test
/// learn the BFF's port *before* constructing config, so `issuer` can be
/// set to the literal base URL the JWKS will be served from.
async fn bind_loopback() -> (TcpListener, SocketAddr) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    (listener, addr)
}

/// Spawn the BFF on a pre-bound listener; detached for the test lifetime.
async fn spawn_bff(listener: TcpListener, cfg: Config) {
    let app = escurel_explore_bff::app(cfg);
    tokio::spawn(serve_on(listener, app));
    tokio::time::sleep(Duration::from_millis(20)).await;
}

/// A throwaway capture backend that stands in for escurel: records the
/// `Authorization` header seen on `POST /mcp` and returns a canned
/// JSON-RPC result.
async fn spawn_capture_backend(captured: Arc<Mutex<Option<String>>>) -> SocketAddr {
    async fn mcp(
        State(captured): State<Arc<Mutex<Option<String>>>>,
        headers: HeaderMap,
        _body: axum::body::Bytes,
    ) -> axum::Json<serde_json::Value> {
        let auth = headers
            .get(axum::http::header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .map(ToOwned::to_owned);
        *captured.lock().unwrap() = auth;
        axum::Json(serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": {"skills": ["greeting"]}
        }))
    }

    let app = Router::new()
        .route("/mcp", post(mcp))
        .route("/ingest", post(mcp))
        .route("/ingest/upload", post(mcp))
        .with_state(captured);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    addr
}

fn config(issuer: &str, backend: &str) -> Config {
    Config {
        bundle_dir: "/nonexistent-bundle".to_owned(),
        backend: backend.to_owned(),
        issuer: issuer.to_owned(),
        audience: "escurel-nonprod".to_owned(),
        tenant: "default".to_owned(),
        kid: "escurel-explore".to_owned(),
        signing_key_pem: None, // ephemeral keypair generated at boot
        groups: Vec::new(),
        groups_claim: "triton_sender_groups".to_owned(),
        admin: false,
    }
}

#[tokio::test]
async fn mints_token_the_real_verifier_accepts() {
    let captured: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let backend_addr = spawn_capture_backend(captured.clone()).await;

    let (listener, addr) = bind_loopback().await;
    let issuer = format!("http://{addr}");
    let cfg = config(&issuer, &format!("http://{backend_addr}"));
    spawn_bff(listener, cfg).await;

    let client = reqwest::Client::new();
    let rpc = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {"name": "list_skills", "arguments": {}}
    });
    let resp = client
        .post(format!("http://{addr}/mcp"))
        .json(&rpc)
        .send()
        .await
        .expect("mcp request");
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["result"]["skills"][0], "greeting");

    // Pull the captured bearer.
    let bearer = captured
        .lock()
        .unwrap()
        .clone()
        .expect("backend saw Authorization");
    let token = bearer.strip_prefix("Bearer ").expect("Bearer scheme");

    // Verify it with the REAL verifier, JWKS fetched from the BFF's /jwks.json.
    let jwks_url = format!("http://{addr}/jwks.json");
    let oidc = OidcConfig::new(issuer.clone(), "escurel-nonprod").with_jwks_uri(jwks_url);
    let verifier = OidcVerifier::new(oidc);
    let ctx = verifier
        .verify(token)
        .await
        .expect("real verifier accepts BFF token");
    assert_eq!(ctx.tenant_id, "default");
    assert_eq!(
        ctx.role,
        Role::Agent,
        "explorer is least-privilege, not admin"
    );
}

#[tokio::test]
async fn mints_configured_groups_under_the_groups_claim() {
    // RBAC: with EXPLORE_BFF_GROUPS set, the minted token carries them
    // under `groups_claim` (here `triton_sender_groups`), so escurel —
    // configured with that groups_claim — projects them into the caller's
    // groups. Asserted through the REAL verifier, not a hand-decode.
    let captured: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let backend_addr = spawn_capture_backend(captured.clone()).await;

    let (listener, addr) = bind_loopback().await;
    let issuer = format!("http://{addr}");
    let mut cfg = config(&issuer, &format!("http://{backend_addr}"));
    cfg.groups = vec!["team-acme".to_owned(), "moderator".to_owned()];
    spawn_bff(listener, cfg).await;

    let client = reqwest::Client::new();
    let rpc = serde_json::json!({
        "jsonrpc": "2.0", "id": 1, "method": "tools/call",
        "params": {"name": "list_skills", "arguments": {}}
    });
    let resp = client
        .post(format!("http://{addr}/mcp"))
        .json(&rpc)
        .send()
        .await
        .expect("mcp request");
    assert_eq!(resp.status(), 200);

    let bearer = captured
        .lock()
        .unwrap()
        .clone()
        .expect("backend saw Authorization");
    let token = bearer.strip_prefix("Bearer ").expect("Bearer scheme");

    let jwks_url = format!("http://{addr}/jwks.json");
    let oidc = OidcConfig::new(issuer.clone(), "escurel-nonprod")
        .with_jwks_uri(jwks_url)
        .with_groups_claim("triton_sender_groups");
    let verifier = OidcVerifier::new(oidc);
    let ctx = verifier.verify(token).await.expect("verifier accepts");
    assert_eq!(ctx.role, Role::Agent, "still least-privilege, not admin");
    assert_eq!(
        ctx.groups,
        vec!["team-acme".to_owned(), "moderator".to_owned()],
        "configured groups projected from the groups_claim"
    );
}

#[tokio::test]
async fn healthz_is_local_and_dependency_free() {
    // Backend points at a dead port — healthz must NOT depend on it.
    let (listener, addr) = bind_loopback().await;
    let cfg = config(&format!("http://{addr}"), "http://127.0.0.1:1");
    spawn_bff(listener, cfg).await;

    let client = reqwest::Client::new();
    let resp = client
        .get(format!("http://{addr}/healthz"))
        .send()
        .await
        .expect("healthz request");
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.text().await.unwrap(), "ok");
}

#[tokio::test]
async fn jwks_endpoint_publishes_the_signing_key() {
    let (listener, addr) = bind_loopback().await;
    let cfg = config(&format!("http://{addr}"), "http://127.0.0.1:1");
    spawn_bff(listener, cfg).await;

    let client = reqwest::Client::new();
    let resp = client
        .get(format!("http://{addr}/jwks.json"))
        .send()
        .await
        .expect("jwks request");
    assert_eq!(resp.status(), 200);
    let jwks: serde_json::Value = resp.json().await.unwrap();
    let keys = jwks["keys"].as_array().expect("keys array");
    assert_eq!(keys.len(), 1);
    assert_eq!(keys[0]["kid"], "escurel-explore");
    assert_eq!(keys[0]["kty"], "RSA");
    assert_eq!(keys[0]["alg"], "RS256");
}

#[tokio::test]
async fn proxies_ingest_upload_with_minted_token() {
    // A2: the BFF forwards /ingest/upload to the backend with a minted bearer
    // (the SPA can't sign a token), same as /mcp.
    let captured: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let backend_addr = spawn_capture_backend(captured.clone()).await;

    let (listener, addr) = bind_loopback().await;
    let issuer = format!("http://{addr}");
    let cfg = config(&issuer, &format!("http://{backend_addr}"));
    spawn_bff(listener, cfg).await;

    let resp = reqwest::Client::new()
        .post(format!("http://{addr}/ingest/upload"))
        .json(&serde_json::json!({ "content_type": "text/plain", "bytes_b64": "aGk=" }))
        .send()
        .await
        .expect("ingest/upload request");
    assert_eq!(resp.status(), 200);

    let bearer = captured
        .lock()
        .unwrap()
        .clone()
        .expect("backend saw Authorization on /ingest/upload");
    assert!(bearer.starts_with("Bearer "), "minted bearer forwarded");
}

#[tokio::test]
async fn admin_mode_mints_admin_role() {
    // Opt-in: with cfg.admin the BFF mints an admin-role token (verified by
    // the real OidcVerifier) so the explorer can drive admin-gated actions.
    let captured: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let backend_addr = spawn_capture_backend(captured.clone()).await;
    let (listener, addr) = bind_loopback().await;
    let issuer = format!("http://{addr}");
    let mut cfg = config(&issuer, &format!("http://{backend_addr}"));
    cfg.admin = true;
    spawn_bff(listener, cfg).await;

    let resp = reqwest::Client::new()
        .post(format!("http://{addr}/mcp"))
        .json(&serde_json::json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"list_skills","arguments":{}}}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let bearer = captured.lock().unwrap().clone().unwrap();
    let token = bearer.strip_prefix("Bearer ").unwrap();
    let oidc = OidcConfig::new(issuer.clone(), "escurel-nonprod")
        .with_jwks_uri(format!("http://{addr}/jwks.json"));
    let ctx = OidcVerifier::new(oidc)
        .verify(token)
        .await
        .expect("verifier accepts");
    assert_eq!(
        ctx.role,
        Role::Admin,
        "admin-mode token projects to Role::Admin"
    );
}
