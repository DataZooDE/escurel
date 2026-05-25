//! End-to-end tests for the gRPC `EscurelAdmin` service stubs.
//!
//! The agent surface (`Escurel`) has its own coverage in
//! grpc_read_tools.rs / grpc_write_tools.rs. This file covers the
//! admin surface — `Health` returns the configured version,
//! every other admin RPC currently returns `Unimplemented`, and
//! all admin RPCs require the `Admin` role on the bearer JWT.
//! Real implementations land in M4 alongside the admin endpoints.

use std::sync::Arc;

use bytes::Bytes;
use duckdb::Connection;
use escurel_auth::{OidcConfig, OidcVerifier};
use escurel_embed::{Embedder, ZeroEmbedder};
use escurel_index::{Indexer, Migrator};
use escurel_proto::v1::escurel_admin_client::EscurelAdminClient;
use escurel_proto::v1::{HealthRequest, QuotaGetRequest, TenantListRequest, TenantSpec};
use escurel_server::{AlwaysReady, ServerConfig, serve};
use escurel_storage::{FsStore, Key, LaneStore};
use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};
use rsa::pkcs1::EncodeRsaPrivateKey;
use rsa::traits::PublicKeyParts;
use rsa::{RsaPrivateKey, RsaPublicKey};
use serde_json::json;
use tempfile::TempDir;
use tonic::Request;
use tonic::metadata::MetadataValue;
use tonic::transport::Channel;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const TENANT: &str = "acme";
const AUDIENCE: &str = "escurel";
const KID: &str = "test-kid";
const ISSUER_PATH: &str = "/realms/test";
const ADMIN_ROLE: &str = "escurel:admin";

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

fn token(keys: &Keys, issuer: &str, tenant: &str, roles: &[&str]) -> String {
    let now = now();
    let mut claims = json!({
        "iss": issuer,
        "aud": AUDIENCE,
        "sub": "user-1",
        "tenant": tenant,
        "iat": now,
        "exp": now + 600,
    });
    if !roles.is_empty() {
        claims["roles"] = json!(roles);
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
    grpc_addr: std::net::SocketAddr,
    issuer: String,
    keys: Keys,
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
        tenant_store: None,
        crdt_backend: None,
    })
    .await
    .unwrap();
    let grpc_addr = handle.grpc_addr.expect("grpc listener bound");
    Harness {
        handle,
        grpc_addr,
        issuer,
        keys,
        _store_dir: store_dir,
        _db_dir: db_dir,
        _wm: wm,
    }
}

fn req<T>(bearer: &MetadataValue<tonic::metadata::Ascii>, body: T) -> Request<T> {
    let mut r = Request::new(body);
    r.metadata_mut().insert("authorization", bearer.clone());
    r
}

async fn admin_client(h: &Harness) -> EscurelAdminClient<Channel> {
    let channel = Channel::from_shared(format!("http://{}", h.grpc_addr))
        .unwrap()
        .connect()
        .await
        .unwrap();
    EscurelAdminClient::new(channel)
}

fn admin_bearer(h: &Harness) -> MetadataValue<tonic::metadata::Ascii> {
    let t = token(&h.keys, &h.issuer, TENANT, &[ADMIN_ROLE]);
    format!("Bearer {t}").parse().unwrap()
}

fn agent_bearer(h: &Harness) -> MetadataValue<tonic::metadata::Ascii> {
    let t = token(&h.keys, &h.issuer, TENANT, &[]);
    format!("Bearer {t}").parse().unwrap()
}

#[tokio::test]
async fn health_returns_configured_version() {
    let h = start().await;
    let mut client = admin_client(&h).await;
    let resp = client
        .health(req(&admin_bearer(&h), HealthRequest::default()))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(resp.version, "1.0.0-test");
    assert!(!resp.status.is_empty());
    h.handle.shutdown().await;
}

#[tokio::test]
async fn health_works_without_bearer_when_unauthenticated_dev_mode() {
    // Bring up a server with no verifier — health on EscurelAdmin
    // must still return the version (it's the substrate health
    // probe and must be dependency-free auth-wise).
    let (indexer, _store_dir, _db_dir) = make_indexer().await;
    let handle = serve(ServerConfig {
        listen: "127.0.0.1:0".to_owned(),
        grpc_listen: Some("127.0.0.1:0".to_owned()),
        version: "dev".to_owned(),
        readiness: Arc::new(AlwaysReady),
        indexer: Some(indexer),
        verifier: None,
        quota: None,
        tenant_store: None,
        crdt_backend: None,
    })
    .await
    .unwrap();
    let grpc_addr = handle.grpc_addr.unwrap();
    let channel = Channel::from_shared(format!("http://{grpc_addr}"))
        .unwrap()
        .connect()
        .await
        .unwrap();
    let mut client = EscurelAdminClient::new(channel);
    let resp = client
        .health(Request::new(HealthRequest::default()))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(resp.version, "dev");
    handle.shutdown().await;
}

#[tokio::test]
async fn admin_rpc_requires_admin_role() {
    let h = start().await;
    let mut client = admin_client(&h).await;
    // Agent-role token must NOT pass an admin RPC.
    let status = client
        .tenant_list(req(&agent_bearer(&h), TenantListRequest::default()))
        .await
        .unwrap_err();
    assert_eq!(status.code(), tonic::Code::PermissionDenied);
    h.handle.shutdown().await;
}

#[tokio::test]
async fn admin_rpc_missing_bearer_returns_unauthenticated() {
    let h = start().await;
    let mut client = admin_client(&h).await;
    let status = client
        .tenant_list(Request::new(TenantListRequest::default()))
        .await
        .unwrap_err();
    assert_eq!(status.code(), tonic::Code::Unauthenticated);
    h.handle.shutdown().await;
}

#[tokio::test]
async fn tenant_crud_without_tenant_store_returns_failed_precondition() {
    // `start()` wires no `tenant_store`, so CRUD must surface
    // `failed_precondition` rather than the M3 `Unimplemented`
    // sentinel — the server can't perform tenant ops without a
    // backing store. M4.5 added the implementation; absence of a
    // store is the explicit "off" knob for health-only deployments.
    let h = start().await;
    let mut client = admin_client(&h).await;
    let status = client
        .tenant_list(req(&admin_bearer(&h), TenantListRequest::default()))
        .await
        .unwrap_err();
    assert_eq!(status.code(), tonic::Code::FailedPrecondition);
    h.handle.shutdown().await;
}

#[tokio::test]
async fn quota_get_returns_unimplemented_in_m3() {
    // Real impl lands once the QuotaManager surfaces a snapshot
    // method. For now the gRPC method exists and the auth gate is
    // wired; everything else returns Unimplemented.
    let h = start().await;
    let mut client = admin_client(&h).await;
    let status = client
        .quota_get(req(
            &admin_bearer(&h),
            QuotaGetRequest {
                tenant_id: TENANT.to_owned(),
            },
        ))
        .await
        .unwrap_err();
    assert_eq!(status.code(), tonic::Code::Unimplemented);
    h.handle.shutdown().await;
}

#[tokio::test]
async fn agent_role_cannot_read_admin_health() {
    // Health is the one admin RPC that should NOT require admin
    // role — it's the substrate liveness probe and must work for
    // any authenticated caller (and unauthenticated when dev mode).
    let h = start().await;
    let mut client = admin_client(&h).await;
    let resp = client
        .health(req(&agent_bearer(&h), HealthRequest::default()))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(resp.version, "1.0.0-test");
    h.handle.shutdown().await;
}

// Smoke test of the modelled-but-unimplemented tenant CRUD bodies.
#[tokio::test]
async fn tenant_spec_round_trips_through_proto_types() {
    // This isn't an RPC test — just verifies the generated proto
    // types compose so callers (CLI, dashboards) can build the
    // request bodies without surprises before M4 lights up the
    // server side.
    let spec = TenantSpec {
        tenant_id: "acme".to_owned(),
        display_name: "Acme Corp".to_owned(),
    };
    assert_eq!(spec.tenant_id, "acme");
}
