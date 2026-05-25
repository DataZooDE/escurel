//! End-to-end tests for the M4.5 admin tenant-CRUD endpoints.
//!
//! These tests stand up a real tonic server with a real
//! `OidcVerifier`, a real wiremock JWKS, real RSA keys, and a real
//! tempdir-backed `FsTenantStore`. No mocks at the boundary —
//! each assertion exercises the production code path verbatim.
//!
//! The test scaffolding (RSA keys, JWKS, token builder) is copied
//! deliberately from `grpc_admin.rs` rather than shared via a
//! `mod common`: integration test files compile independently and
//! `mod common` triggers a "module compiled twice" warning chain
//! when each test binary picks it up.

use std::path::PathBuf;
use std::sync::Arc;

use bytes::Bytes;
use duckdb::Connection;
use escurel_admin::{FsTenantStore, TenantStore};
use escurel_auth::{OidcConfig, OidcVerifier};
use escurel_embed::{Embedder, ZeroEmbedder};
use escurel_index::{Indexer, Migrator};
use escurel_proto::v1::escurel_admin_client::EscurelAdminClient;
use escurel_proto::v1::{
    AuditRequest, TenantCreateRequest, TenantDeleteRequest, TenantGetRequest, TenantListRequest,
    TenantSpec, TenantUpdateRequest,
};
use escurel_server::{AlwaysReady, ServerConfig, ServerHandle, serve};
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

async fn make_indexer_for(tenant: &str) -> (Arc<Indexer>, TempDir, TempDir) {
    let store_dir = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    let store: Arc<dyn LaneStore> = Arc::new(FsStore::new(store_dir.path().to_path_buf()));
    let embedder: Arc<dyn Embedder> = Arc::new(ZeroEmbedder::default());
    let conn = Connection::open(db_dir.path().join("escurel.duckdb")).unwrap();
    Migrator::up(&conn).unwrap();
    let indexer = Arc::new(Indexer::new(Arc::clone(&store), embedder, conn, tenant).unwrap());
    let body = "---\ntype: skill\nid: customer\ndescription: x\n---\n# customer\n";
    let key = Key::new(tenant, "markdown/skills/customer.md".to_owned()).unwrap();
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
    handle: ServerHandle,
    grpc_addr: std::net::SocketAddr,
    issuer: String,
    keys: Keys,
    tenants_root: PathBuf,
    _tenants_dir: TempDir,
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

    let (indexer, store_dir, db_dir) = make_indexer_for(TENANT).await;
    let tenants_dir = TempDir::new().unwrap();
    let tenants_root = tenants_dir.path().to_path_buf();
    let tenant_store: Arc<dyn TenantStore> = Arc::new(FsTenantStore::new(tenants_root.clone()));

    let handle = serve(ServerConfig {
        listen: "127.0.0.1:0".to_owned(),
        grpc_listen: Some("127.0.0.1:0".to_owned()),
        version: "1.0.0-test".to_owned(),
        readiness: Arc::new(AlwaysReady),
        indexer: Some(indexer),
        verifier: Some(verifier),
        quota: None,
        tenant_store: Some(tenant_store),
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
        tenants_root,
        _tenants_dir: tenants_dir,
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

fn spec(id: &str, name: &str) -> TenantSpec {
    TenantSpec {
        tenant_id: id.to_owned(),
        display_name: name.to_owned(),
    }
}

// --- tests ---------------------------------------------------------

#[tokio::test]
async fn tenant_create_seeds_directory_and_duckdb_file() {
    let h = start().await;
    let mut client = admin_client(&h).await;
    let resp = client
        .tenant_create(req(
            &admin_bearer(&h),
            TenantCreateRequest {
                spec: Some(spec("acme", "Acme Corp")),
            },
        ))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(resp.spec.as_ref().unwrap().tenant_id, "acme");
    assert_eq!(resp.spec.as_ref().unwrap().display_name, "Acme Corp");
    let dir = h.tenants_root.join("acme");
    assert!(dir.join("tenant.json").is_file());
    assert!(dir.join("markdown").is_dir());
    assert!(dir.join("db").join("escurel.duckdb").is_file());
    h.handle.shutdown().await;
}

#[tokio::test]
async fn tenant_create_rejects_invalid_id() {
    let h = start().await;
    let mut client = admin_client(&h).await;
    let status = client
        .tenant_create(req(
            &admin_bearer(&h),
            TenantCreateRequest {
                spec: Some(spec("Bad/Id", "x")),
            },
        ))
        .await
        .unwrap_err();
    assert_eq!(status.code(), tonic::Code::InvalidArgument);
    h.handle.shutdown().await;
}

#[tokio::test]
async fn tenant_create_returns_already_exists_for_duplicate() {
    let h = start().await;
    let mut client = admin_client(&h).await;
    client
        .tenant_create(req(
            &admin_bearer(&h),
            TenantCreateRequest {
                spec: Some(spec("acme", "Acme")),
            },
        ))
        .await
        .unwrap();
    let status = client
        .tenant_create(req(
            &admin_bearer(&h),
            TenantCreateRequest {
                spec: Some(spec("acme", "Acme Again")),
            },
        ))
        .await
        .unwrap_err();
    assert_eq!(status.code(), tonic::Code::AlreadyExists);
    h.handle.shutdown().await;
}

#[tokio::test]
async fn tenant_list_returns_created_tenants() {
    let h = start().await;
    let mut client = admin_client(&h).await;
    for id in ["acme", "globex"] {
        client
            .tenant_create(req(
                &admin_bearer(&h),
                TenantCreateRequest {
                    spec: Some(spec(id, id)),
                },
            ))
            .await
            .unwrap();
    }
    let resp = client
        .tenant_list(req(&admin_bearer(&h), TenantListRequest::default()))
        .await
        .unwrap()
        .into_inner();
    let mut ids: Vec<String> = resp.tenants.into_iter().map(|t| t.tenant_id).collect();
    ids.sort();
    assert_eq!(ids, vec!["acme".to_owned(), "globex".to_owned()]);
    h.handle.shutdown().await;
}

#[tokio::test]
async fn tenant_get_returns_spec_for_existing_tenant() {
    let h = start().await;
    let mut client = admin_client(&h).await;
    client
        .tenant_create(req(
            &admin_bearer(&h),
            TenantCreateRequest {
                spec: Some(spec("acme", "Acme Corp")),
            },
        ))
        .await
        .unwrap();
    let resp = client
        .tenant_get(req(
            &admin_bearer(&h),
            TenantGetRequest {
                tenant_id: "acme".to_owned(),
            },
        ))
        .await
        .unwrap()
        .into_inner();
    let s = resp.spec.unwrap();
    assert_eq!(s.tenant_id, "acme");
    assert_eq!(s.display_name, "Acme Corp");
    h.handle.shutdown().await;
}

#[tokio::test]
async fn tenant_get_returns_not_found_for_missing_tenant() {
    let h = start().await;
    let mut client = admin_client(&h).await;
    let status = client
        .tenant_get(req(
            &admin_bearer(&h),
            TenantGetRequest {
                tenant_id: "ghost".to_owned(),
            },
        ))
        .await
        .unwrap_err();
    assert_eq!(status.code(), tonic::Code::NotFound);
    h.handle.shutdown().await;
}

#[tokio::test]
async fn tenant_update_changes_display_name() {
    let h = start().await;
    let mut client = admin_client(&h).await;
    client
        .tenant_create(req(
            &admin_bearer(&h),
            TenantCreateRequest {
                spec: Some(spec("acme", "Old")),
            },
        ))
        .await
        .unwrap();
    let resp = client
        .tenant_update(req(
            &admin_bearer(&h),
            TenantUpdateRequest {
                spec: Some(spec("acme", "New Name")),
            },
        ))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(resp.spec.as_ref().unwrap().display_name, "New Name");
    // Re-read via get to confirm persistence.
    let got = client
        .tenant_get(req(
            &admin_bearer(&h),
            TenantGetRequest {
                tenant_id: "acme".to_owned(),
            },
        ))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(got.spec.unwrap().display_name, "New Name");
    h.handle.shutdown().await;
}

#[tokio::test]
async fn tenant_delete_removes_directory_and_returns_true() {
    let h = start().await;
    let mut client = admin_client(&h).await;
    client
        .tenant_create(req(
            &admin_bearer(&h),
            TenantCreateRequest {
                spec: Some(spec("acme", "Acme")),
            },
        ))
        .await
        .unwrap();
    let dir = h.tenants_root.join("acme");
    assert!(dir.exists());
    let resp = client
        .tenant_delete(req(
            &admin_bearer(&h),
            TenantDeleteRequest {
                tenant_id: "acme".to_owned(),
            },
        ))
        .await
        .unwrap()
        .into_inner();
    assert!(resp.deleted);
    assert!(!dir.exists());
    h.handle.shutdown().await;
}

#[tokio::test]
async fn tenant_delete_returns_false_for_missing_tenant() {
    let h = start().await;
    let mut client = admin_client(&h).await;
    let resp = client
        .tenant_delete(req(
            &admin_bearer(&h),
            TenantDeleteRequest {
                tenant_id: "ghost".to_owned(),
            },
        ))
        .await
        .unwrap()
        .into_inner();
    assert!(!resp.deleted);
    h.handle.shutdown().await;
}

#[tokio::test]
async fn audit_returns_clean_drift_for_seeded_tenant() {
    let h = start().await;
    let mut client = admin_client(&h).await;
    let resp = client
        .audit(req(
            &admin_bearer(&h),
            AuditRequest {
                tenant_id: TENANT.to_owned(),
                scope: String::new(),
            },
        ))
        .await
        .unwrap()
        .into_inner();
    assert!(resp.markdown_not_in_duckdb.is_empty());
    assert!(resp.indexed_but_no_markdown.is_empty());
    h.handle.shutdown().await;
}

#[tokio::test]
async fn audit_rejects_mismatched_tenant() {
    let h = start().await;
    let mut client = admin_client(&h).await;
    let status = client
        .audit(req(
            &admin_bearer(&h),
            AuditRequest {
                tenant_id: "other-tenant".to_owned(),
                scope: String::new(),
            },
        ))
        .await
        .unwrap_err();
    assert_eq!(status.code(), tonic::Code::FailedPrecondition);
    h.handle.shutdown().await;
}

#[tokio::test]
async fn admin_role_still_required_for_crud() {
    let h = start().await;
    let mut client = admin_client(&h).await;
    let status = client
        .tenant_create(req(
            &agent_bearer(&h),
            TenantCreateRequest {
                spec: Some(spec("acme", "x")),
            },
        ))
        .await
        .unwrap_err();
    assert_eq!(status.code(), tonic::Code::PermissionDenied);
    h.handle.shutdown().await;
}
