//! End-to-end tests for the gRPC write tools and the remaining
//! read tools: `search`, `neighbours`, `run_stored_query`,
//! `update_page`.
//!
//! Real Indexer (DuckDB + FsStore + ZeroEmbedder), real tonic
//! server on a random port, real tonic client, real OidcVerifier
//! against a wiremock JWKS endpoint with a freshly-generated
//! 2048-bit RSA pair, real QuotaManager.

use std::sync::Arc;

use bytes::Bytes;
use duckdb::Connection;
use escurel_auth::{OidcConfig, OidcVerifier};
use escurel_embed::{Embedder, ZeroEmbedder};
use escurel_index::{Indexer, Migrator};
use escurel_proto::v1::escurel_client::EscurelClient;
use escurel_proto::v1::{
    NeighboursRequest, RunStoredQueryRequest, SearchRequest, UpdatePageRequest,
};
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

const QUERY_SKILL: &str = "---\n\
type: skill\n\
id: query\n\
description: SQL view over the indexed corpus.\n\
---\n\
# query\n";

const COUNT_QUERY: &str = "---\n\
type: instance\n\
skill: query\n\
id: count-customers\n\
db: relational\n\
sql: \"SELECT count(*) AS n FROM pages WHERE skill = 'customer' AND page_type = 'instance'\"\n\
---\n\
# count-customers\n";

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
        ("markdown/skills/query.md", QUERY_SKILL),
        ("markdown/instances/customer/acme.md", ACME_INSTANCE),
        ("markdown/instances/customer/initech.md", INITECH_INSTANCE),
        ("markdown/instances/query/count-customers.md", COUNT_QUERY),
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
        tenant_store: None,
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
async fn search_returns_hits_for_query() {
    let h = start(None).await;
    let mut a = authed_client(&h).await;
    let resp = a
        .client
        .search(a.req(SearchRequest {
            q: "Acme".to_owned(),
            k: 5,
            granularity: String::new(),
            page_type: String::new(),
            skill: String::new(),
            filter_json: String::new(),
        }))
        .await
        .unwrap()
        .into_inner();
    assert!(!resp.hits.is_empty(), "expected at least one search hit");
    assert_eq!(resp.granularity, "block");
    assert!(resp.hits.iter().any(|h| h.skill == "customer"));
    h.handle.shutdown().await;
}

#[tokio::test]
async fn neighbours_returns_outbound_edges() {
    let h = start(None).await;
    let mut a = authed_client(&h).await;
    let resp = a
        .client
        .neighbours(a.req(NeighboursRequest {
            page_id: "markdown/instances/customer/acme.md".to_owned(),
            direction: "out".to_owned(),
            link_skill: String::new(),
            link_skill_in: Vec::new(),
            order_by: String::new(),
            limit: 0,
        }))
        .await
        .unwrap()
        .into_inner();
    assert!(
        resp.edges.iter().any(|e| e.dst_page == "initech"),
        "expected acme → initech edge, got: {:?}",
        resp.edges
    );
    h.handle.shutdown().await;
}

#[tokio::test]
async fn run_stored_query_executes_count() {
    let h = start(None).await;
    let mut a = authed_client(&h).await;
    let resp = a
        .client
        .run_stored_query(a.req(RunStoredQueryRequest {
            query_id: "count-customers".to_owned(),
            params_json: String::new(),
        }))
        .await
        .unwrap()
        .into_inner();
    let rows: serde_json::Value = serde_json::from_str(&resp.rows_json).unwrap();
    assert_eq!(rows[0]["n"], 2);
    assert!(resp.schema.iter().any(|c| c.name == "n"));
    h.handle.shutdown().await;
}

#[tokio::test]
async fn update_page_round_trips_through_grpc() {
    let h = start(None).await;
    let mut a = authed_client(&h).await;
    let new_body = "---\n\
                    type: instance\n\
                    skill: customer\n\
                    id: globex\n\
                    name: Globex\n\
                    ---\n\
                    # Globex\n";
    let resp = a
        .client
        .update_page(a.req(UpdatePageRequest {
            page_id: "markdown/instances/customer/globex.md".to_owned(),
            content: new_body.to_owned(),
        }))
        .await
        .unwrap()
        .into_inner();
    assert!(resp.ok);
    assert_eq!(resp.new_version, "v1");
    assert!(resp.issues.is_empty());
    h.handle.shutdown().await;
}

#[tokio::test]
async fn update_page_debits_writes_dimension_independently() {
    // Writes bucket is 1/min so the first update_page passes, the
    // second is rejected. A read tool (Queries dimension) should
    // still succeed because its bucket is untouched.
    let q = QuotaConfig {
        queries_per_minute: 60,
        writes_per_minute: 1,
        embeds_per_minute: 60,
        concurrent_sessions: 32,
    };
    let h = start(Some(Arc::new(QuotaManager::new(q)))).await;
    let mut a = authed_client(&h).await;
    let body = "---\n\
                type: instance\n\
                skill: customer\n\
                id: tmp\n\
                name: tmp\n\
                ---\n\
                # tmp\n";
    a.client
        .update_page(a.req(UpdatePageRequest {
            page_id: "markdown/instances/customer/tmp.md".to_owned(),
            content: body.to_owned(),
        }))
        .await
        .unwrap();
    let err = a
        .client
        .update_page(a.req(UpdatePageRequest {
            page_id: "markdown/instances/customer/tmp2.md".to_owned(),
            content: body.to_owned(),
        }))
        .await
        .unwrap_err();
    assert_eq!(err.code(), tonic::Code::ResourceExhausted);

    // The Queries bucket is independent — a read should still pass.
    a.client
        .neighbours(a.req(NeighboursRequest {
            page_id: "markdown/instances/customer/acme.md".to_owned(),
            direction: "out".to_owned(),
            link_skill: String::new(),
            link_skill_in: Vec::new(),
            order_by: String::new(),
            limit: 0,
        }))
        .await
        .unwrap();
    h.handle.shutdown().await;
}

#[tokio::test]
async fn search_rejects_invalid_page_type() {
    let h = start(None).await;
    let mut a = authed_client(&h).await;
    let err = a
        .client
        .search(a.req(SearchRequest {
            q: "x".to_owned(),
            k: 1,
            granularity: String::new(),
            page_type: "bogus".to_owned(),
            skill: String::new(),
            filter_json: String::new(),
        }))
        .await
        .unwrap_err();
    assert_eq!(err.code(), tonic::Code::InvalidArgument);
    h.handle.shutdown().await;
}
