//! End-to-end tests for the `escurel-client` typed wrapper.
//!
//! Real gateway (`escurel_server::serve`), real tonic transport,
//! real `OidcVerifier` against a wiremock JWKS endpoint with a
//! freshly-generated 2048-bit RSA pair, real `Indexer` with a real
//! DuckDB file. No mocks at the boundary the test exercises (CLAUDE
//! principle 2).
//!
//! The auth/indexer harness is copied from
//! `crates/escurel-server/tests/grpc_read_tools.rs`; M-DX-2 hoists
//! it into `escurel-test-support` and M-DX-3 retires the copy.

use std::sync::Arc;

use bytes::Bytes;
use duckdb::Connection;
use escurel_auth::{OidcConfig, OidcVerifier};
use escurel_client::{
    Client, ExpandRequest, ListSkillsRequest, ResolveRequest, SearchRequest, SecretString,
    UpdatePageRequest,
};
use escurel_embed::{Embedder, ZeroEmbedder};
use escurel_index::{Indexer, Migrator};
use escurel_server::{AlwaysReady, ServerConfig, serve};
use escurel_storage::{FsStore, Key, LaneStore};
use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};
use rsa::pkcs1::EncodeRsaPrivateKey;
use rsa::traits::PublicKeyParts;
use rsa::{RsaPrivateKey, RsaPublicKey};
use serde_json::json;
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

impl Harness {
    fn endpoint(&self) -> String {
        format!("http://{}", self.grpc_addr)
    }

    fn token(&self) -> String {
        token(&self.keys, &self.issuer, TENANT)
    }
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

async fn authed_client(h: &Harness) -> Client {
    Client::connect(&h.endpoint(), SecretString::from(h.token()))
        .await
        .unwrap()
}

#[tokio::test]
async fn connect_succeeds_against_running_gateway() {
    let h = start().await;
    // A successful connect + a trivial RPC proves the channel is
    // up and the token is being threaded through.
    let client = authed_client(&h).await;
    client
        .list_skills(ListSkillsRequest::default())
        .await
        .unwrap();
    h.handle.shutdown().await;
}

#[tokio::test]
async fn list_skills_round_trips() {
    let h = start().await;
    let client = authed_client(&h).await;
    let resp = client
        .list_skills(ListSkillsRequest::default())
        .await
        .unwrap();
    assert_eq!(resp.skills.len(), 1);
    assert_eq!(resp.skills[0].id, "customer");
    assert_eq!(resp.skills[0].description, "A buying organisation.");
    h.handle.shutdown().await;
}

#[tokio::test]
async fn resolve_round_trips() {
    let h = start().await;
    let client = authed_client(&h).await;
    let resp = client
        .resolve(ResolveRequest {
            wikilink: "[[customer::acme]]".to_owned(),
        })
        .await
        .unwrap();
    assert!(resp.exists);
    let page = resp.page.expect("page present");
    assert_eq!(page.skill, "customer");
    assert_eq!(page.slug, "acme");
    h.handle.shutdown().await;
}

#[tokio::test]
async fn expand_round_trips() {
    let h = start().await;
    let client = authed_client(&h).await;
    let resolved = client
        .resolve(ResolveRequest {
            wikilink: "[[customer::acme]]".to_owned(),
        })
        .await
        .unwrap();
    let page_id = resolved.page.unwrap().page_id;
    let resp = client
        .expand(ExpandRequest {
            page_id,
            anchor: String::new(),
            version: String::new(),
        })
        .await
        .unwrap();
    assert!(!resp.body.is_empty());
    assert!(resp.wikilinks_out.iter().any(|w| w.id == "initech"));
    h.handle.shutdown().await;
}

#[tokio::test]
async fn search_round_trips() {
    let h = start().await;
    let client = authed_client(&h).await;
    // ZeroEmbedder + FTS-backed search; query the seeded body text.
    let resp = client
        .search(SearchRequest {
            q: "Acme".to_owned(),
            k: 5,
            granularity: String::new(),
            page_type: String::new(),
            skill: String::new(),
            filter_json: String::new(),
        })
        .await
        .unwrap();
    // The response shape is what the contract commits to — the
    // surface returns whatever the indexer ranked. Asserting on
    // `granularity` is the cheapest stable invariant.
    assert_eq!(resp.granularity, "block");
    h.handle.shutdown().await;
}

#[tokio::test]
async fn update_page_round_trips() {
    let h = start().await;
    let client = authed_client(&h).await;
    let body = "---\n\
type: instance\n\
skill: customer\n\
id: globex\n\
name: Globex\n\
---\n\
# Globex\n";
    let resp = client
        .update_page(UpdatePageRequest {
            page_id: "markdown/instances/customer/globex.md".to_owned(),
            content: body.to_owned(),
        })
        .await
        .unwrap();
    assert!(resp.ok, "update_page should succeed: {resp:?}");
    h.handle.shutdown().await;
}

#[tokio::test]
async fn missing_token_surfaces_unauthenticated_error() {
    let h = start().await;
    // Bogus token: parses fine as a header but the verifier rejects
    // it. Surface should be `Error::Rpc` carrying
    // `Code::Unauthenticated`.
    let client = Client::connect(
        &h.endpoint(),
        SecretString::from("not.a.real.jwt".to_owned()),
    )
    .await
    .unwrap();
    let err = client
        .list_skills(ListSkillsRequest::default())
        .await
        .unwrap_err();
    match err {
        escurel_client::Error::Rpc(status) => {
            assert_eq!(
                status.code(),
                tonic::Code::Unauthenticated,
                "status: {status:?}"
            );
        }
        other => panic!("expected Error::Rpc(Unauthenticated), got {other:?}"),
    }
    h.handle.shutdown().await;
}

#[tokio::test]
async fn invalid_endpoint_url_returns_error() {
    // Not a URL at all — must surface as `InvalidEndpoint`, never
    // as a panic, never as a connect-timeout.
    let err = Client::connect("not a url", SecretString::from("x".to_owned()))
        .await
        .unwrap_err();
    assert!(
        matches!(err, escurel_client::Error::InvalidEndpoint(_)),
        "got {err:?}"
    );
}

#[tokio::test]
async fn token_is_not_leaked_in_debug_output() {
    // The secret marker must not appear in any `{:?}` formatting of
    // the client. This is the only mechanical check we have against
    // accidental log leaks.
    let secret = "THIS_TOKEN_SHOULD_NEVER_APPEAR_IN_LOGS_xyz123";
    let h = start().await;
    let client = Client::connect(&h.endpoint(), SecretString::from(secret.to_owned()))
        .await
        .unwrap();
    let dbg = format!("{client:?}");
    assert!(
        !dbg.contains(secret),
        "bearer token leaked into Debug output: {dbg}"
    );
    h.handle.shutdown().await;
}
