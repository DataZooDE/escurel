//! End-to-end test for the `escurel` CLI.
//!
//! Spins up the real gateway (HTTP + gRPC + OidcVerifier) on
//! random ports, then exercises every CLI subcommand via the
//! compiled binary (`assert_cmd::cargo_bin`). No mocks at the
//! CLI boundary; the only test double is the wiremock JWKS
//! endpoint feeding the verifier.

use std::sync::Arc;

use assert_cmd::Command;
use bytes::Bytes;
use duckdb::Connection;
use escurel_auth::{OidcConfig, OidcVerifier};
use escurel_embed::{Embedder, ZeroEmbedder};
use escurel_index::{Indexer, Migrator};
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

fn token(keys: &Keys, issuer: &str, tenant: &str) -> String {
    let now = now();
    let claims = json!({
        "iss": issuer,
        "aud": AUDIENCE,
        "sub": "user-1",
        "tenant": tenant,
        "iat": now,
        "exp": now + 600,
    });
    let mut header = Header::new(Algorithm::RS256);
    header.kid = Some(KID.to_owned());
    let key = EncodingKey::from_rsa_pem(&keys.private_pem).unwrap();
    encode(&header, &claims, &key).unwrap()
}

const CUSTOMER_SKILL: &str = "---\n\
type: skill\n\
id: customer\n\
description: A buying organisation.\n\
required_frontmatter: [id, name]\n\
optional_frontmatter: [tier]\n\
---\n\
# customer\n";

const ACME_INSTANCE: &str = "---\n\
type: instance\n\
skill: customer\n\
id: acme\n\
name: Acme Corp\n\
tier: gold\n\
---\n\
# Acme Corp\n\nKey account. See [[customer::initech]].\n";

const INITECH_INSTANCE: &str = "---\n\
type: instance\n\
skill: customer\n\
id: initech\n\
name: Initech\n\
---\n\
# Initech\n";

async fn make_indexer() -> (Arc<Indexer>, TempDir, TempDir) {
    let store_dir = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    let store: Arc<dyn LaneStore> = Arc::new(FsStore::new(store_dir.path().to_path_buf()));
    let embedder: Arc<dyn Embedder> = Arc::new(ZeroEmbedder::default());
    let conn = Connection::open(db_dir.path().join("escurel.duckdb")).unwrap();
    Migrator::up(&conn).unwrap();
    let indexer = Arc::new(Indexer::new(Arc::clone(&store), embedder, conn, TENANT).unwrap());
    for (rel, body) in [
        ("markdown/skills/customer.md", CUSTOMER_SKILL),
        ("markdown/instances/customer/acme.md", ACME_INSTANCE),
        ("markdown/instances/customer/initech.md", INITECH_INSTANCE),
    ] {
        let key = Key::new(TENANT, rel.to_owned()).unwrap();
        store
            .write(&key, Bytes::copy_from_slice(body.as_bytes()))
            .await
            .unwrap();
        indexer.update_page(rel, body).await.unwrap();
    }
    indexer.refresh_fts().await.unwrap();
    (indexer, store_dir, db_dir)
}

struct Harness {
    handle: escurel_server::ServerHandle,
    grpc_addr: std::net::SocketAddr,
    bearer: String,
    _store_dir: TempDir,
    _db_dir: TempDir,
    _wm: MockServer,
}

async fn start() -> Harness {
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
        grpc_listen: Some("127.0.0.1:0".to_owned()),
        version: "1.0.0-test".to_owned(),
        readiness: Arc::new(AlwaysReady),
        indexer: Some(indexer),
        verifier: Some(verifier),
        quota: None,
    })
    .await
    .unwrap();
    let grpc_addr = handle.grpc_addr.expect("grpc listener bound");
    let bearer = token(&keys, &issuer, TENANT);
    Harness {
        handle,
        grpc_addr,
        bearer,
        _store_dir: store_dir,
        _db_dir: db_dir,
        _wm: wm,
    }
}

fn cli(h: &Harness) -> Command {
    let mut c = Command::cargo_bin("escurel").expect("escurel binary built");
    c.env("ESCUREL_SERVER", format!("http://{}", h.grpc_addr))
        .env("ESCUREL_TOKEN", &h.bearer);
    c
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn list_skills_emits_seeded_skill() {
    let h = start().await;
    let assert = tokio::task::spawn_blocking({
        let h_addr = h.grpc_addr;
        let h_bearer = h.bearer.clone();
        move || {
            Command::cargo_bin("escurel")
                .unwrap()
                .env("ESCUREL_SERVER", format!("http://{h_addr}"))
                .env("ESCUREL_TOKEN", h_bearer)
                .args(["list-skills"])
                .assert()
                .success()
        }
    })
    .await
    .unwrap();
    let out: Value = serde_json::from_slice(&assert.get_output().stdout).unwrap();
    let skills = out["skills"].as_array().unwrap();
    assert!(skills.iter().any(|s| s["id"] == "customer"));
    h.handle.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resolve_emits_existing_page() {
    let h = start().await;
    let assert = tokio::task::spawn_blocking({
        let mut c = cli(&h);
        c.args(["resolve", "[[customer::acme]]"]);
        move || c.assert().success()
    })
    .await
    .unwrap();
    let out: Value = serde_json::from_slice(&assert.get_output().stdout).unwrap();
    assert_eq!(out["exists"], true);
    assert_eq!(out["page"]["skill"], "customer");
    assert_eq!(out["page"]["slug"], "acme");
    h.handle.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn expand_emits_body_and_wikilinks() {
    let h = start().await;
    // Use list-instances to find the page_id.
    let inst_out = tokio::task::spawn_blocking({
        let mut c = cli(&h);
        c.args(["list-instances", "--skill", "customer"]);
        move || c.assert().success()
    })
    .await
    .unwrap();
    let inst: Value = serde_json::from_slice(&inst_out.get_output().stdout).unwrap();
    let acme = inst["instances"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["page_id"].as_str().unwrap().contains("acme"))
        .unwrap()
        .clone();
    let page_id = acme["page_id"].as_str().unwrap().to_owned();

    let expand_out = tokio::task::spawn_blocking({
        let mut c = cli(&h);
        c.args(["expand", page_id.as_str()]);
        move || c.assert().success()
    })
    .await
    .unwrap();
    let out: Value = serde_json::from_slice(&expand_out.get_output().stdout).unwrap();
    assert!(out["body"].as_str().unwrap().contains("Acme Corp"));
    assert!(
        out["wikilinks_out"]
            .as_array()
            .unwrap()
            .iter()
            .any(|w| w["id"] == "initech")
    );
    h.handle.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn search_emits_hits() {
    let h = start().await;
    let assert = tokio::task::spawn_blocking({
        let mut c = cli(&h);
        c.args(["search", "Acme", "--k", "5"]);
        move || c.assert().success()
    })
    .await
    .unwrap();
    let out: Value = serde_json::from_slice(&assert.get_output().stdout).unwrap();
    let hits = out["hits"].as_array().unwrap();
    assert!(!hits.is_empty());
    assert_eq!(out["granularity"], "block");
    h.handle.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn update_page_via_stdin_round_trips() {
    let h = start().await;
    let body = "---\n\
                type: instance\n\
                skill: customer\n\
                id: globex\n\
                name: Globex\n\
                ---\n\
                # Globex\n";
    let assert = tokio::task::spawn_blocking({
        let mut c = cli(&h);
        c.args(["update-page", "markdown/instances/customer/globex.md"]);
        c.write_stdin(body);
        move || c.assert().success()
    })
    .await
    .unwrap();
    let out: Value = serde_json::from_slice(&assert.get_output().stdout).unwrap();
    assert_eq!(out["ok"], true);
    h.handle.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn missing_token_returns_unauthenticated_error() {
    let h = start().await;
    let output = tokio::task::spawn_blocking({
        let h_addr = h.grpc_addr;
        move || {
            Command::cargo_bin("escurel")
                .unwrap()
                .env("ESCUREL_SERVER", format!("http://{h_addr}"))
                .env_remove("ESCUREL_TOKEN")
                .args(["list-skills"])
                .assert()
                .failure()
        }
    })
    .await
    .unwrap();
    let stderr = String::from_utf8_lossy(&output.get_output().stderr).to_string();
    assert!(
        stderr.to_lowercase().contains("unauthenticated")
            || stderr.to_lowercase().contains("missing")
            || stderr.contains("ESCUREL_TOKEN"),
        "expected an auth-related error in stderr, got: {stderr}"
    );
    h.handle.shutdown().await;
}
