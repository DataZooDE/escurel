//! End-to-end tests for the auth + quota middleware on /mcp.
//!
//! Real gateway, real Indexer (DuckDB + FsStore + ZeroEmbedder),
//! real OidcVerifier against a wiremock JWKS endpoint with a
//! freshly-generated 2048-bit RSA pair, real QuotaManager.

use std::sync::Arc;

use bytes::Bytes;
use duckdb::Connection;
use escurel_auth::{OidcConfig, OidcVerifier};
use escurel_embed::{Embedder, ZeroEmbedder};
use escurel_index::{Indexer, Migrator};
use escurel_quota::{QuotaConfig, QuotaManager};
use escurel_server::{AlwaysReady, ServerConfig, serve};
use escurel_storage::{FsStore, Key, LaneStore};
use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};
use rsa::pkcs1::EncodeRsaPrivateKey;
use rsa::traits::PublicKeyParts;
use rsa::{RsaPrivateKey, RsaPublicKey};
use serde_json::{Value, json};
use tempfile::TempDir;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const TENANT: &str = "acme";
const AUDIENCE: &str = "escurel";
const KID: &str = "test-kid";
const ISSUER_PATH: &str = "/realms/test";

struct Keys {
    private_pem: Vec<u8>,
    n_b64: String,
    e_b64: String,
}

fn keys() -> Keys {
    let mut rng = rand::thread_rng();
    let private = RsaPrivateKey::new(&mut rng, 2048).unwrap();
    let public = RsaPublicKey::from(&private);
    let private_pem = private
        .to_pkcs1_pem(rsa::pkcs1::LineEnding::LF)
        .unwrap()
        .as_bytes()
        .to_vec();
    Keys {
        private_pem,
        n_b64: b64url(&public.n().to_bytes_be()),
        e_b64: b64url(&public.e().to_bytes_be()),
    }
}

fn b64url(b: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b)
}

fn now() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

async fn jwks_mock(server: &MockServer, k: &Keys) {
    let jwks = json!({
        "keys": [{
            "kid": KID, "kty": "RSA", "alg": "RS256", "use": "sig",
            "n": k.n_b64, "e": k.e_b64,
        }]
    });
    Mock::given(method("GET"))
        .and(path(format!("{ISSUER_PATH}/protocol/openid-connect/certs")))
        .respond_with(ResponseTemplate::new(200).set_body_json(jwks))
        .mount(server)
        .await;
}

fn token(keys: &Keys, issuer: &str, tenant: &str, role: Option<&str>) -> String {
    let now = now();
    let mut claims = json!({
        "iss": issuer,
        "aud": AUDIENCE,
        "sub": "user-1",
        "tenant": tenant,
        "iat": now,
        "exp": now + 600,
    });
    if let Some(r) = role {
        claims["roles"] = json!([r]);
    }
    let mut header = Header::new(Algorithm::RS256);
    header.kid = Some(KID.to_owned());
    let key = EncodingKey::from_rsa_pem(&keys.private_pem).unwrap();
    encode(&header, &claims, &key).unwrap()
}

async fn make_indexer() -> (Arc<Indexer>, TempDir, TempDir) {
    let store_dir = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    let store: Arc<dyn LaneStore> = Arc::new(FsStore::new(store_dir.path().to_path_buf()));
    let embedder: Arc<dyn Embedder> = Arc::new(ZeroEmbedder::default());
    let conn = Connection::open(db_dir.path().join("escurel.duckdb")).unwrap();
    Migrator::up(&conn).unwrap();
    let indexer = Arc::new(Indexer::new(Arc::clone(&store), embedder, conn, TENANT).unwrap());

    // Minimal seed so list_skills returns something.
    let body = "---\ntype: skill\nid: customer\ndescription: x\n---\n# customer\n";
    let key = Key::new(TENANT, "markdown/skills/customer.md".to_owned()).unwrap();
    store
        .write(&key, Bytes::from_static(body.as_bytes()))
        .await
        .unwrap();
    indexer
        .update_page("markdown/skills/customer.md", body)
        .await
        .unwrap();

    (indexer, store_dir, db_dir)
}

struct Harness {
    handle: escurel_server::ServerHandle,
    client: reqwest::Client,
    base_url: String,
    issuer: String,
    keys: Keys,
    // Keep ownership of these so they outlive the test.
    _store_dir: TempDir,
    _db_dir: TempDir,
    _wm: MockServer,
}

async fn start_authed(quota: Option<Arc<QuotaManager>>) -> Harness {
    let wm = MockServer::start().await;
    let keys = keys();
    jwks_mock(&wm, &keys).await;
    let issuer = format!("{}{ISSUER_PATH}", wm.uri());
    let cfg = OidcConfig::new(issuer.clone(), AUDIENCE.to_owned())
        .with_jwks_uri(format!("{issuer}/protocol/openid-connect/certs"));
    let verifier = Arc::new(OidcVerifier::new(cfg));

    let (indexer, store_dir, db_dir) = make_indexer().await;

    let handle = serve(ServerConfig {
        listen: "127.0.0.1:0".to_owned(),
        version: "1.0.0-test".to_owned(),
        readiness: Arc::new(AlwaysReady),
        indexer: Some(indexer),
        verifier: Some(verifier),
        quota,
    })
    .await
    .unwrap();
    let base_url = format!("http://{}", handle.local_addr);
    Harness {
        handle,
        client: reqwest::Client::new(),
        base_url,
        issuer,
        keys,
        _store_dir: store_dir,
        _db_dir: db_dir,
        _wm: wm,
    }
}

async fn post_mcp(h: &Harness, bearer: Option<&str>, body: Value) -> reqwest::Response {
    let mut req = h.client.post(format!("{}/mcp", h.base_url)).json(&body);
    if let Some(t) = bearer {
        req = req.header("authorization", format!("Bearer {t}"));
    }
    req.send().await.unwrap()
}

fn list_skills_call() -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": { "name": "list_skills", "arguments": {} }
    })
}

#[tokio::test]
async fn missing_bearer_returns_401() {
    let h = start_authed(None).await;
    let resp = post_mcp(&h, None, list_skills_call()).await;
    assert_eq!(resp.status(), 401);
    h.handle.shutdown().await;
}

#[tokio::test]
async fn bearer_without_prefix_returns_401() {
    let h = start_authed(None).await;
    let valid = token(&h.keys, &h.issuer, TENANT, None);
    let resp = h
        .client
        .post(format!("{}/mcp", h.base_url))
        .header("authorization", valid) // missing "Bearer " prefix
        .json(&list_skills_call())
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);
    h.handle.shutdown().await;
}

#[tokio::test]
async fn invalid_token_returns_401() {
    let h = start_authed(None).await;
    let resp = post_mcp(&h, Some("not.a.real.jwt"), list_skills_call()).await;
    assert_eq!(resp.status(), 401);
    h.handle.shutdown().await;
}

#[tokio::test]
async fn valid_token_lets_request_through() {
    let h = start_authed(None).await;
    let t = token(&h.keys, &h.issuer, TENANT, None);
    let resp = post_mcp(&h, Some(&t), list_skills_call()).await;
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert!(body["result"]["skills"].is_array());
    h.handle.shutdown().await;
}

#[tokio::test]
async fn quota_exhaustion_returns_429_with_retry_after_header() {
    let q = QuotaConfig {
        queries_per_minute: 1, // 1 per minute
        writes_per_minute: 60,
        embeds_per_minute: 60,
        concurrent_sessions: 32,
    };
    let h = start_authed(Some(Arc::new(QuotaManager::new(q)))).await;
    let t = token(&h.keys, &h.issuer, TENANT, None);

    // First call succeeds.
    let resp = post_mcp(&h, Some(&t), list_skills_call()).await;
    assert_eq!(resp.status(), 200);

    // Second call exhausts.
    let resp = post_mcp(&h, Some(&t), list_skills_call()).await;
    assert_eq!(resp.status(), 429);
    let retry = resp
        .headers()
        .get("Retry-After-Ms")
        .map(|v| v.to_str().unwrap().to_owned());
    assert!(retry.is_some(), "Retry-After-Ms header must be present");
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["error"]["code"], -32000);

    h.handle.shutdown().await;
}

#[tokio::test]
async fn write_tool_debits_writes_dimension_independently() {
    // Quotas: 60 queries/min, 1 write/min.
    let q = QuotaConfig {
        queries_per_minute: 60,
        writes_per_minute: 1,
        embeds_per_minute: 60,
        concurrent_sessions: 32,
    };
    let h = start_authed(Some(Arc::new(QuotaManager::new(q)))).await;
    let t = token(&h.keys, &h.issuer, TENANT, None);

    let write_body = "---\ntype: instance\nskill: customer\nid: one\n---\n# One\n";
    let write_call = json!({
        "jsonrpc": "2.0", "id": 1, "method": "tools/call",
        "params": { "name": "update_page", "arguments": {
            "page_id": "markdown/instances/customer/one.md",
            "content": write_body,
        }}
    });

    // First write succeeds; second exhausts.
    assert_eq!(
        post_mcp(&h, Some(&t), write_call.clone()).await.status(),
        200
    );
    let resp = post_mcp(
        &h,
        Some(&t),
        json!({
            "jsonrpc": "2.0", "id": 2, "method": "tools/call",
            "params": { "name": "update_page", "arguments": {
                "page_id": "markdown/instances/customer/two.md",
                "content": write_body.replace("one", "two"),
            }}
        }),
    )
    .await;
    assert_eq!(resp.status(), 429);

    // But a read still goes through (independent bucket).
    let resp = post_mcp(&h, Some(&t), list_skills_call()).await;
    assert_eq!(resp.status(), 200);

    h.handle.shutdown().await;
}

#[tokio::test]
async fn tools_list_does_not_debit_quota() {
    let q = QuotaConfig {
        queries_per_minute: 1, // only 1 query budget
        writes_per_minute: 60,
        embeds_per_minute: 60,
        concurrent_sessions: 32,
    };
    let h = start_authed(Some(Arc::new(QuotaManager::new(q)))).await;
    let t = token(&h.keys, &h.issuer, TENANT, None);

    // tools/list should not debit; we can call it 5 times.
    for _ in 0..5 {
        let resp = post_mcp(
            &h,
            Some(&t),
            json!({
                "jsonrpc": "2.0", "id": 1, "method": "tools/list"
            }),
        )
        .await;
        assert_eq!(resp.status(), 200, "tools/list should not be rate-limited");
    }
    // After 5 tools/list, the queries bucket is still fresh and a
    // single tools/call can succeed.
    let resp = post_mcp(&h, Some(&t), list_skills_call()).await;
    assert_eq!(resp.status(), 200);

    h.handle.shutdown().await;
}

#[tokio::test]
async fn tenants_have_independent_quota_state() {
    let q = QuotaConfig {
        queries_per_minute: 1,
        writes_per_minute: 60,
        embeds_per_minute: 60,
        concurrent_sessions: 32,
    };
    let h = start_authed(Some(Arc::new(QuotaManager::new(q)))).await;

    let t_acme = token(&h.keys, &h.issuer, "acme", None);
    let t_globex = token(&h.keys, &h.issuer, "globex", None);

    assert_eq!(
        post_mcp(&h, Some(&t_acme), list_skills_call())
            .await
            .status(),
        200
    );
    assert_eq!(
        post_mcp(&h, Some(&t_acme), list_skills_call())
            .await
            .status(),
        429
    );
    // Globex's bucket is still fresh.
    assert_eq!(
        post_mcp(&h, Some(&t_globex), list_skills_call())
            .await
            .status(),
        200
    );

    h.handle.shutdown().await;
}
