//! End-to-end tests for the M4.5b streaming + small admin RPCs.
//!
//! Exercised against a real tonic server, a real `OidcVerifier`
//! plus wiremock JWKS, real RSA keys, real `Indexer` over a real
//! DuckDB file, and a real `FsTenantStore` backing the export /
//! import bytes. The harness is intentionally copy-pasted from
//! `grpc_admin_crud.rs` rather than shared via `mod common` —
//! integration test binaries compile independently and the shared
//! module pattern fires "module compiled twice" warning chains.
//!
//! Covered surface:
//!
//! * `rebuild` — server-streaming, one `RebuildProgress` chunk
//!   per page, terminator chunk with `done == total`.
//! * `tenant_export` / `tenant_import` — round-trip via real
//!   tar+gz frames over the wire.
//! * `quota_get` — bucket-floor remaining tokens reflect recent
//!   debits.
//! * `embedding_reload` — placeholder revision string for M5.
//! * Role gate — agent-role token cannot drive Rebuild.

use std::path::PathBuf;
use std::sync::Arc;

use bytes::Bytes;
use duckdb::Connection;
use escurel_admin::{FsTenantStore, TenantSpec as AdminTenantSpec, TenantStore};
use escurel_auth::{OidcConfig, OidcVerifier};
use escurel_embed::{Embedder, ZeroEmbedder};
use escurel_index::{Indexer, Migrator};
use escurel_proto::v1::escurel_admin_client::EscurelAdminClient;
use escurel_proto::v1::escurel_client::EscurelClient;
use escurel_proto::v1::{
    EmbeddingReloadRequest, ListSkillsRequest, QuotaGetRequest, RebuildRequest,
    TenantCreateRequest, TenantExportRequest, TenantImportChunk, TenantSpec,
};
use escurel_quota::{QuotaConfig, QuotaManager};
use escurel_server::{AlwaysReady, ServerConfig, ServerHandle, serve};
use escurel_storage::{FsStore, Key, LaneStore};
use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};
use rsa::pkcs1::EncodeRsaPrivateKey;
use rsa::traits::PublicKeyParts;
use rsa::{RsaPrivateKey, RsaPublicKey};
use serde_json::json;
use tempfile::TempDir;
use tokio_stream::StreamExt;
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

const PAGES: &[(&str, &str)] = &[
    (
        "markdown/skills/customer.md",
        "---\ntype: skill\nid: customer\ndescription: x\n---\n# customer\n",
    ),
    (
        "markdown/skills/order.md",
        "---\ntype: skill\nid: order\ndescription: y\n---\n# order\n",
    ),
    (
        "markdown/instances/customer/acme.md",
        "---\ntype: instance\nskill: customer\nid: acme\n---\n# Acme\n",
    ),
];

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

/// Seed a multi-page tenant: writes both the markdown files (via
/// LaneStore, which is what `Indexer::rebuild` walks) and the
/// `<tenants_root>/<TENANT>/markdown/...` files on disk (which is
/// what `tenant_export` walks). The two trees deliberately mirror
/// each other so the rebuild and export surfaces see the same set
/// of pages.
async fn seed_tenant(
    tenants_root: &std::path::Path,
    store: &Arc<dyn LaneStore>,
    indexer: &Indexer,
) {
    let md_root = tenants_root.join(TENANT).join("markdown");
    for (path, body) in PAGES {
        let key = Key::new(TENANT, (*path).to_owned()).unwrap();
        store
            .write(&key, Bytes::from_static(body.as_bytes()))
            .await
            .unwrap();
        let abs = md_root.join(path.strip_prefix("markdown/").unwrap());
        if let Some(parent) = abs.parent() {
            tokio::fs::create_dir_all(parent).await.unwrap();
        }
        tokio::fs::write(&abs, body).await.unwrap();
        indexer.update_page(path, body).await.unwrap();
    }
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

    let store_dir = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    let store: Arc<dyn LaneStore> = Arc::new(FsStore::new(store_dir.path().to_path_buf()));
    let embedder: Arc<dyn Embedder> = Arc::new(ZeroEmbedder::default());
    let conn = Connection::open(db_dir.path().join("escurel.duckdb")).unwrap();
    Migrator::up(&conn).unwrap();
    let indexer = Arc::new(Indexer::new(Arc::clone(&store), embedder, conn, TENANT).unwrap());

    let tenants_dir = TempDir::new().unwrap();
    let tenants_root = tenants_dir.path().to_path_buf();
    let tenant_store: Arc<dyn TenantStore> = Arc::new(FsTenantStore::new(tenants_root.clone()));
    // Provision the tenant in the FsTenantStore so its `markdown/`
    // subtree is present for the export RPC to walk.
    tenant_store
        .create(&AdminTenantSpec {
            tenant_id: TENANT.to_owned(),
            display_name: "Acme".to_owned(),
        })
        .await
        .unwrap();
    seed_tenant(&tenants_root, &store, &indexer).await;

    let quota = Arc::new(QuotaManager::new(QuotaConfig::defaults()));

    let handle = serve(ServerConfig {
        listen: "127.0.0.1:0".to_owned(),
        grpc_listen: Some("127.0.0.1:0".to_owned()),
        version: "1.0.0-test".to_owned(),
        readiness: Arc::new(AlwaysReady),
        indexer: Some(indexer),
        verifier: Some(verifier),
        quota: Some(quota),
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

async fn agent_client(h: &Harness) -> EscurelClient<Channel> {
    let channel = Channel::from_shared(format!("http://{}", h.grpc_addr))
        .unwrap()
        .connect()
        .await
        .unwrap();
    EscurelClient::new(channel)
}

fn admin_bearer(h: &Harness) -> MetadataValue<tonic::metadata::Ascii> {
    let t = token(&h.keys, &h.issuer, TENANT, &[ADMIN_ROLE]);
    format!("Bearer {t}").parse().unwrap()
}

fn agent_bearer(h: &Harness) -> MetadataValue<tonic::metadata::Ascii> {
    let t = token(&h.keys, &h.issuer, TENANT, &[]);
    format!("Bearer {t}").parse().unwrap()
}

// --- rebuild --------------------------------------------------------

#[tokio::test]
async fn rebuild_streams_one_progress_chunk_per_page() {
    let h = start().await;
    let mut client = admin_client(&h).await;
    let mut stream = client
        .rebuild(req(
            &admin_bearer(&h),
            RebuildRequest {
                tenant_id: TENANT.to_owned(),
                scope: String::new(),
            },
        ))
        .await
        .unwrap()
        .into_inner();
    let mut chunks = Vec::new();
    while let Some(msg) = stream.next().await {
        chunks.push(msg.unwrap());
    }
    assert_eq!(
        chunks.len(),
        PAGES.len(),
        "expected one progress chunk per page; got {chunks:?}"
    );
    h.handle.shutdown().await;
}

#[tokio::test]
async fn rebuild_emits_final_chunk_with_done_equal_total() {
    let h = start().await;
    let mut client = admin_client(&h).await;
    let mut stream = client
        .rebuild(req(
            &admin_bearer(&h),
            RebuildRequest {
                tenant_id: TENANT.to_owned(),
                scope: String::new(),
            },
        ))
        .await
        .unwrap()
        .into_inner();
    let mut last = None;
    while let Some(msg) = stream.next().await {
        last = Some(msg.unwrap());
    }
    let last = last.expect("at least one progress chunk");
    assert_eq!(last.total, PAGES.len() as u64);
    assert_eq!(last.done, last.total);
    assert!(
        !last.current_page.is_empty(),
        "current_page must be set on every chunk"
    );
    h.handle.shutdown().await;
}

// --- export / import -----------------------------------------------

async fn drain_export(
    client: &mut EscurelAdminClient<Channel>,
    h: &Harness,
    tenant: &str,
) -> Vec<u8> {
    let mut stream = client
        .tenant_export(req(
            &admin_bearer(h),
            TenantExportRequest {
                tenant_id: tenant.to_owned(),
            },
        ))
        .await
        .unwrap()
        .into_inner();
    let mut out = Vec::new();
    while let Some(chunk) = stream.next().await {
        out.extend_from_slice(&chunk.unwrap().data);
    }
    out
}

#[tokio::test]
async fn tenant_export_streams_tarball_chunks() {
    let h = start().await;
    let mut client = admin_client(&h).await;
    let bytes = drain_export(&mut client, &h, TENANT).await;
    assert!(!bytes.is_empty(), "export must produce at least one chunk");
    // gzip magic bytes — `flate2`'s default encoder writes the
    // standard gzip header.
    assert_eq!(
        &bytes[..2],
        b"\x1f\x8b",
        "exported stream must be gzip-framed"
    );
    h.handle.shutdown().await;
}

#[tokio::test]
async fn tenant_export_round_trips_through_tenant_import() {
    let h = start().await;
    let mut client = admin_client(&h).await;
    // Pull the tarball bytes via the streaming export RPC.
    let bytes = drain_export(&mut client, &h, TENANT).await;

    // Provision a fresh tenant in the same backing store, then
    // pipe the bytes back through TenantImport. The round-trip
    // proves we use real tar+gz on both sides.
    let target = "globex";
    client
        .tenant_create(req(
            &admin_bearer(&h),
            TenantCreateRequest {
                spec: Some(TenantSpec {
                    tenant_id: target.to_owned(),
                    display_name: "Globex".to_owned(),
                }),
            },
        ))
        .await
        .unwrap();

    let chunks: Vec<TenantImportChunk> = bytes
        .chunks(32 * 1024)
        .map(|c| TenantImportChunk {
            tenant_id: target.to_owned(),
            data: c.to_vec(),
        })
        .collect();
    let resp = client
        .tenant_import(req(&admin_bearer(&h), tokio_stream::iter(chunks)))
        .await
        .unwrap()
        .into_inner();
    assert!(resp.bytes_imported > 0);

    // Every seeded markdown file must be present under the new
    // tenant's `markdown/` tree on disk.
    for (path, _) in PAGES {
        let rel = path.strip_prefix("markdown/").unwrap();
        let abs = h.tenants_root.join(target).join("markdown").join(rel);
        assert!(abs.is_file(), "import did not restore `{}`", abs.display());
    }
    h.handle.shutdown().await;
}

/// Codex P2 (PR M4.5b): `tenant_export` skipped tenant_id
/// validation before invoking `TenantStore::tenant_dir`, which is
/// `root.join(tenant_id)`. A crafted `../other` would have walked
/// up into a sibling tenant's markdown tree. Fix validates ids up
/// front; this test pins the gate.
#[tokio::test]
async fn tenant_export_rejects_path_traversal_tenant_id() {
    let h = start().await;
    let mut client = admin_client(&h).await;
    let err = client
        .tenant_export(req(
            &admin_bearer(&h),
            TenantExportRequest {
                tenant_id: format!("../{TENANT}"),
            },
        ))
        .await
        .unwrap_err();
    assert_eq!(err.code(), tonic::Code::InvalidArgument);
    h.handle.shutdown().await;
}

#[tokio::test]
async fn tenant_import_rejects_unknown_tenant() {
    let h = start().await;
    let mut client = admin_client(&h).await;
    let chunks = vec![TenantImportChunk {
        tenant_id: "ghost".to_owned(),
        data: vec![0_u8; 10],
    }];
    let status = client
        .tenant_import(req(&admin_bearer(&h), tokio_stream::iter(chunks)))
        .await
        .unwrap_err();
    assert_eq!(status.code(), tonic::Code::NotFound);
    h.handle.shutdown().await;
}

// --- quota_get ------------------------------------------------------

#[tokio::test]
async fn quota_get_returns_remaining_tokens() {
    let h = start().await;
    let mut client = admin_client(&h).await;
    let resp = client
        .quota_get(req(
            &admin_bearer(&h),
            QuotaGetRequest {
                tenant_id: TENANT.to_owned(),
            },
        ))
        .await
        .unwrap()
        .into_inner();
    let defaults = QuotaConfig::defaults();
    assert_eq!(resp.queries_remaining, defaults.queries_per_minute);
    assert_eq!(resp.writes_remaining, defaults.writes_per_minute);
    assert_eq!(resp.embeds_remaining, defaults.embeds_per_minute);
    // Field name in the proto is `concurrent_sessions`, but the
    // semantics we ship are "currently occupied" so the
    // baseline reads as zero.
    assert_eq!(resp.concurrent_sessions, 0);
    h.handle.shutdown().await;
}

#[tokio::test]
async fn quota_get_reflects_recent_debits() {
    let h = start().await;
    let mut agent = agent_client(&h).await;
    // Burn a few `queries`-bucket tokens via the real agent
    // surface. The agent client carries a tenant-claim that
    // matches the quota manager's tenant key.
    let agent_bearer_md = agent_bearer(&h);
    for _ in 0..3_u32 {
        let mut r = Request::new(ListSkillsRequest::default());
        r.metadata_mut()
            .insert("authorization", agent_bearer_md.clone());
        agent.list_skills(r).await.unwrap();
    }

    let mut admin = admin_client(&h).await;
    let resp = admin
        .quota_get(req(
            &admin_bearer(&h),
            QuotaGetRequest {
                tenant_id: TENANT.to_owned(),
            },
        ))
        .await
        .unwrap()
        .into_inner();
    let defaults = QuotaConfig::defaults();
    assert!(
        resp.queries_remaining <= defaults.queries_per_minute - 3,
        "expected at least 3 tokens consumed; got {}",
        resp.queries_remaining
    );
    h.handle.shutdown().await;
}

// --- embedding_reload placeholder ----------------------------------

#[tokio::test]
async fn embedding_reload_returns_placeholder_revision() {
    let h = start().await;
    let mut client = admin_client(&h).await;
    let resp = client
        .embedding_reload(req(&admin_bearer(&h), EmbeddingReloadRequest::default()))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(resp.model_revision, "M5");
    h.handle.shutdown().await;
}

// --- role gate ------------------------------------------------------

#[tokio::test]
async fn streaming_admin_rpcs_still_require_admin_role() {
    let h = start().await;
    let mut client = admin_client(&h).await;
    let status = client
        .rebuild(req(
            &agent_bearer(&h),
            RebuildRequest {
                tenant_id: TENANT.to_owned(),
                scope: String::new(),
            },
        ))
        .await
        .unwrap_err();
    assert_eq!(status.code(), tonic::Code::PermissionDenied);
    h.handle.shutdown().await;
}
