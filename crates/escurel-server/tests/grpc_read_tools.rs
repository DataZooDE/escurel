//! End-to-end tests for the gRPC `Escurel` read-tools surface.
//! Real Indexer (DuckDB + FsStore + ZeroEmbedder), real tonic
//! server on a random port, real tonic client, real OidcVerifier
//! against a wiremock JWKS endpoint with a freshly-generated 2048-bit
//! RSA pair, real QuotaManager.

use std::sync::Arc;

use bytes::Bytes;
use duckdb::Connection;
use escurel_auth::{OidcConfig, OidcVerifier};
use escurel_embed::{Embedder, ZeroEmbedder};
use escurel_index::{Indexer, Migrator};
use escurel_proto::v1::escurel_client::EscurelClient;
use escurel_proto::v1::{ExpandRequest, ListInstancesRequest, ListSkillsRequest, ResolveRequest};
use escurel_quota::{QuotaConfig, QuotaManager};
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

async fn make_indexer() -> (Arc<Indexer>, TempDir, TempDir) {
    let store_dir = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    let store: Arc<dyn LaneStore> = Arc::new(FsStore::new(store_dir.path().to_path_buf()));
    let embedder: Arc<dyn Embedder> = Arc::new(ZeroEmbedder::default());
    let conn = Connection::open(db_dir.path().join("escurel.duckdb")).unwrap();
    Migrator::up(&conn).unwrap();
    let indexer = Arc::new(Indexer::new(Arc::clone(&store), embedder, conn, TENANT).unwrap());

    // Seed a skill + two instances so the read tools have something
    // to show. Bodies are minimal; the read path doesn't care about
    // the body content beyond what the indexer parses.
    seed(
        &store,
        &indexer,
        "markdown/skills/customer.md",
        CUSTOMER_SKILL,
    )
    .await;
    seed(
        &store,
        &indexer,
        "markdown/instances/customer/acme.md",
        ACME_INSTANCE,
    )
    .await;
    seed(
        &store,
        &indexer,
        "markdown/instances/customer/initech.md",
        INITECH_INSTANCE,
    )
    .await;

    (indexer, store_dir, db_dir)
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

async fn seed(store: &Arc<dyn LaneStore>, indexer: &Indexer, rel: &str, body: &str) {
    let key = Key::new(TENANT, rel.to_owned()).unwrap();
    store
        .write(&key, Bytes::copy_from_slice(body.as_bytes()))
        .await
        .unwrap();
    indexer.update_page(rel, body).await.unwrap();
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

async fn start(quota: Option<Arc<QuotaManager>>) -> Harness {
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
        quota,
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

struct Authed {
    client: EscurelClient<Channel>,
    bearer: MetadataValue<tonic::metadata::Ascii>,
}

impl Authed {
    fn req<T>(&self, body: T) -> Request<T> {
        let mut r = Request::new(body);
        r.metadata_mut()
            .insert("authorization", self.bearer.clone());
        r
    }
}

async fn authed_client(h: &Harness) -> Authed {
    let channel = Channel::from_shared(format!("http://{}", h.grpc_addr))
        .unwrap()
        .connect()
        .await
        .unwrap();
    let t = token(&h.keys, &h.issuer, TENANT);
    let bearer: MetadataValue<_> = format!("Bearer {t}").parse().unwrap();
    Authed {
        client: EscurelClient::new(channel),
        bearer,
    }
}

#[tokio::test]
async fn list_skills_returns_seeded_skill() {
    let h = start(None).await;
    let mut a = authed_client(&h).await;
    let resp = a
        .client
        .list_skills(a.req(ListSkillsRequest::default()))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(resp.skills.len(), 1);
    let s = &resp.skills[0];
    assert_eq!(s.id, "customer");
    assert_eq!(s.description, "A buying organisation.");
    assert!(s.required_frontmatter.contains(&"id".to_owned()));
    assert!(s.optional_frontmatter.contains(&"tier".to_owned()));
    assert!(!s.is_event_typed);
    h.handle.shutdown().await;
}

#[tokio::test]
async fn list_instances_returns_seeded_instances() {
    let h = start(None).await;
    let mut a = authed_client(&h).await;
    let resp = a
        .client
        .list_instances(a.req(ListInstancesRequest {
            skill: "customer".to_owned(),
            order_by_at: String::new(),
            limit: 0,
        }))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(resp.instances.len(), 2);
    let ids: Vec<_> = resp.instances.iter().map(|i| i.skill.clone()).collect();
    assert!(ids.iter().all(|s| s == "customer"));
    assert!(
        resp.instances
            .iter()
            .all(|i| !i.frontmatter_json.is_empty())
    );
    h.handle.shutdown().await;
}

#[tokio::test]
async fn resolve_returns_existing_page() {
    let h = start(None).await;
    let mut a = authed_client(&h).await;
    let resp = a
        .client
        .resolve(a.req(ResolveRequest {
            wikilink: "[[customer::acme]]".to_owned(),
        }))
        .await
        .unwrap()
        .into_inner();
    assert!(resp.exists);
    let parsed = resp.parsed.unwrap();
    assert_eq!(parsed.skill, "customer");
    assert_eq!(parsed.id, "acme");
    let page = resp.page.unwrap();
    assert_eq!(page.skill, "customer");
    assert_eq!(page.slug, "acme");
    assert_eq!(page.page_type, "instance");
    h.handle.shutdown().await;
}

#[tokio::test]
async fn expand_returns_body_and_outbound_wikilinks() {
    let h = start(None).await;
    let mut a = authed_client(&h).await;
    let resolved = a
        .client
        .resolve(a.req(ResolveRequest {
            wikilink: "[[customer::acme]]".to_owned(),
        }))
        .await
        .unwrap()
        .into_inner();
    let page_id = resolved.page.unwrap().page_id;
    let resp = a
        .client
        .expand(a.req(ExpandRequest {
            page_id,
            anchor: String::new(),
            version: String::new(),
        }))
        .await
        .unwrap()
        .into_inner();
    let page = resp.page.unwrap();
    assert_eq!(page.skill, "customer");
    assert!(!resp.body.is_empty());
    assert!(!resp.blocks.is_empty());
    assert!(resp.wikilinks_out.iter().any(|w| w.id == "initech"));
    h.handle.shutdown().await;
}

#[tokio::test]
async fn missing_bearer_returns_unauthenticated() {
    let h = start(None).await;
    let channel = Channel::from_shared(format!("http://{}", h.grpc_addr))
        .unwrap()
        .connect()
        .await
        .unwrap();
    let mut client = EscurelClient::new(channel);
    let status = client
        .list_skills(ListSkillsRequest::default())
        .await
        .unwrap_err();
    assert_eq!(status.code(), tonic::Code::Unauthenticated);
    h.handle.shutdown().await;
}

#[tokio::test]
async fn invalid_token_returns_unauthenticated() {
    let h = start(None).await;
    let channel = Channel::from_shared(format!("http://{}", h.grpc_addr))
        .unwrap()
        .connect()
        .await
        .unwrap();
    let metadata: MetadataValue<_> = "Bearer not.a.real.jwt".parse().unwrap();
    let mut client = EscurelClient::new(channel);
    let mut req = Request::new(ListSkillsRequest::default());
    req.metadata_mut().insert("authorization", metadata);
    let status = client.list_skills(req).await.unwrap_err();
    assert_eq!(status.code(), tonic::Code::Unauthenticated);
    h.handle.shutdown().await;
}

#[tokio::test]
async fn quota_exhaustion_returns_resource_exhausted() {
    let q = QuotaConfig {
        queries_per_minute: 1,
        writes_per_minute: 60,
        embeds_per_minute: 60,
        concurrent_sessions: 32,
    };
    let h = start(Some(Arc::new(QuotaManager::new(q)))).await;
    let mut a = authed_client(&h).await;
    // First call passes.
    a.client
        .list_skills(a.req(ListSkillsRequest::default()))
        .await
        .unwrap();
    // Second call hits the empty bucket.
    let status = a
        .client
        .list_skills(a.req(ListSkillsRequest::default()))
        .await
        .unwrap_err();
    assert_eq!(status.code(), tonic::Code::ResourceExhausted);
    let retry = status
        .metadata()
        .get("retry-after-ms")
        .expect("retry-after-ms metadata present");
    assert!(retry.to_str().unwrap().parse::<u64>().unwrap() > 0);
    h.handle.shutdown().await;
}
