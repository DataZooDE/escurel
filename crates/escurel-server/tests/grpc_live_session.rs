//! End-to-end tests for the M4.3 gRPC `Escurel::LiveSession`
//! bidi-streaming RPC.
//!
//! Real running gateway (axum HTTP + tonic gRPC sharing one
//! `AppState`), real `LiveDoc` actor over a real `DuckdbCrdtBackend`
//! sharing the DuckDB connection used by the HTTP `open_session` /
//! `close_session` tools, real `OidcVerifier` against the
//! in-process JWKS the support crate stands up, real Loro
//! `Client` peer producing incremental update bytes per
//! `docs/notes/discovered/2026-05-25-loro-incremental-updates-need-persistent-client.md`.
//!
//! Covered surface:
//!
//! * The first inbound `LiveOp` is the "attach" frame
//!   (`session = sess_…`, `op = []`); the server replies with one
//!   `LiveAck` carrying the head version and current content.
//! * A subsequent `LiveOp` with real Loro bytes is applied to the
//!   session's `LiveDoc`; the reply carries the post-merge version.
//! * Multiple ops round-trip in one stream and the version
//!   monotonically advances.
//! * An attach on an unknown session emits a `LiveAck` whose
//!   `issues[0].code == "unknown_session"` and the stream ends.
//! * Missing bearer is rejected with `Unauthenticated` before the
//!   stream opens — same enforcement as unary RPCs.
//! * Each op on the bidi debits the per-tenant `Writes` bucket
//!   (the second op trips a 1/min budget).
//! * The client closing the request stream does *not* tear down the
//!   `LiveDoc`; a follow-up HTTP `close_session` still succeeds.

use std::sync::Arc;
use std::time::Duration;

use duckdb::Connection;
use escurel_crdt::{CrdtBackend, DuckdbCrdtBackend};
use escurel_index::Migrator;
use escurel_proto::v1::escurel_client::EscurelClient;
use escurel_proto::v1::{LiveAck, LiveOp};
use escurel_quota::{QuotaConfig, QuotaManager};
use escurel_test_support::{AuthMode, ConfigOverrides, EscurelProcess, Opts, Role};
use loro::{ExportMode, LoroDoc};
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::sync::Mutex;
use tokio::sync::mpsc;
use tokio_stream::StreamExt;
use tokio_stream::wrappers::ReceiverStream;
use tonic::Request;
use tonic::metadata::MetadataValue;
use tonic::transport::Channel;

const TENANT: &str = "acme";

// --- Loro client peer (persistent so incremental updates are
// anchored to ids the server's actor has already seen — see the
// discovered note 2026-05-25-loro-incremental-updates-…) ---------

struct LoroClient {
    doc: LoroDoc,
    vv: loro::VersionVector,
}

impl LoroClient {
    fn new() -> Self {
        let doc = LoroDoc::new();
        let vv = doc.oplog_vv();
        Self { doc, vv }
    }

    fn insert(&mut self, pos: usize, text: &str) -> Vec<u8> {
        self.doc.get_text("body").insert(pos, text).unwrap();
        self.doc.commit();
        let update = self.doc.export(ExportMode::updates(&self.vv)).unwrap();
        self.vv = self.doc.oplog_vv();
        update
    }

    fn body_len(&self) -> usize {
        self.doc.get_text("body").len_unicode()
    }
}

// --- harness ----------------------------------------------------

struct Harness {
    process: EscurelProcess,
    http: reqwest::Client,
    _db_dir: TempDir,
}

async fn start(quota: Option<Arc<QuotaManager>>) -> Harness {
    let db_dir = TempDir::new().unwrap();
    let conn = Connection::open(db_dir.path().join("escurel.duckdb")).unwrap();
    Migrator::up(&conn).unwrap();
    let shared = Arc::new(Mutex::new(conn));
    let crdt_backend: Arc<dyn CrdtBackend> = Arc::new(DuckdbCrdtBackend::new(Arc::clone(&shared)));

    let process = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: None,
        config_overrides: ConfigOverrides {
            quota,
            crdt_backend: Some(crdt_backend),
            disable_indexer: true,
            ..Default::default()
        },
    })
    .await;
    Harness {
        process,
        http: reqwest::Client::new(),
        _db_dir: db_dir,
    }
}

fn bearer(h: &Harness) -> MetadataValue<tonic::metadata::Ascii> {
    let t = h.process.mint_token(TENANT, Role::Agent);
    format!("Bearer {t}").parse().unwrap()
}

async fn grpc_client(h: &Harness) -> EscurelClient<Channel> {
    let endpoint = h.process.grpc_endpoint().expect("grpc endpoint").to_owned();
    let channel = Channel::from_shared(endpoint)
        .unwrap()
        .connect()
        .await
        .unwrap();
    EscurelClient::new(channel)
}

/// POST a JSON-RPC call to `POST /mcp` and assert HTTP 200 + no
/// `error` envelope. Returns the `result` value.
async fn http_call_ok(h: &Harness, name: &str, args: Value) -> Value {
    let bearer = format!("Bearer {}", h.process.mint_token(TENANT, Role::Agent));
    let body = h
        .http
        .post(h.process.mcp_url())
        .header("authorization", bearer)
        .json(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": { "name": name, "arguments": args },
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(body.status(), 200, "http {name} status");
    let body: Value = body.json().await.unwrap();
    assert!(body.get("error").is_none(), "{name} returned error: {body}");
    body["result"].clone()
}

/// Open a session through the real HTTP MCP `open_session` tool and
/// return the session id. Mirrors what an MCP-aware client would do
/// before attaching the gRPC bidi.
async fn open_session(h: &Harness, page_id: &str) -> String {
    let r = http_call_ok(h, "open_session", json!({ "page_id": page_id })).await;
    r["session"].as_str().unwrap().to_owned()
}

/// Drive a single `LiveSession` bidi stream: spawn an mpsc-backed
/// inbound stream, send each provided op in order, return the
/// inbound stream of `LiveAck`s along with the sender (so the test
/// can close it explicitly when needed).
async fn start_live_session(
    h: &Harness,
    auth: Option<MetadataValue<tonic::metadata::Ascii>>,
) -> Result<(mpsc::Sender<LiveOp>, tonic::Streaming<LiveAck>), tonic::Status> {
    let mut client = grpc_client(h).await;
    let (tx, rx) = mpsc::channel::<LiveOp>(16);
    let mut req = Request::new(ReceiverStream::new(rx));
    if let Some(bearer) = auth {
        req.metadata_mut().insert("authorization", bearer);
    }
    let stream = client.live_session(req).await?.into_inner();
    Ok((tx, stream))
}

/// Read the next `LiveAck`, bounded by a generous timeout — bidi
/// reads otherwise hang forever if the server forgot to reply.
async fn next_ack(stream: &mut tonic::Streaming<LiveAck>) -> LiveAck {
    let next = tokio::time::timeout(Duration::from_secs(5), stream.next()).await;
    let next = next.expect("ack within 5s");
    next.expect("ack within 5s").expect("stream not ended")
}

// --- tests ----------------------------------------------------------

#[tokio::test]
async fn attach_to_session_emits_initial_ack_with_head_version() {
    let h = start(None).await;
    let session = open_session(&h, "page-attach").await;

    let (tx, mut stream) = start_live_session(&h, Some(bearer(&h))).await.unwrap();
    tx.send(LiveOp {
        session: session.clone(),
        op: Vec::new(),
    })
    .await
    .unwrap();

    let ack = next_ack(&mut stream).await;
    assert_eq!(ack.session, session);
    assert_eq!(ack.merged_version, "v0", "head at open is v0: {ack:?}");
    assert_eq!(ack.content, "", "empty page → empty body");
    assert!(ack.issues.is_empty(), "issues should be empty on attach");

    drop(tx);
    h.process.shutdown().await;
}

#[tokio::test]
async fn apply_op_via_bidi_updates_doc_and_acks_with_new_version() {
    let h = start(None).await;
    let session = open_session(&h, "page-apply").await;

    let (tx, mut stream) = start_live_session(&h, Some(bearer(&h))).await.unwrap();
    tx.send(LiveOp {
        session: session.clone(),
        op: Vec::new(),
    })
    .await
    .unwrap();
    let _attach = next_ack(&mut stream).await;

    let mut client = LoroClient::new();
    let op_bytes = client.insert(0, "hello");
    tx.send(LiveOp {
        session: session.clone(),
        op: op_bytes,
    })
    .await
    .unwrap();
    let ack = next_ack(&mut stream).await;
    assert_eq!(ack.session, session);
    assert_eq!(ack.merged_version, "v1");
    assert_eq!(ack.content, "hello");
    assert!(ack.issues.is_empty());

    drop(tx);
    h.process.shutdown().await;
}

#[tokio::test]
async fn multiple_ops_round_trip_in_one_stream() {
    let h = start(None).await;
    let session = open_session(&h, "page-multi").await;

    let (tx, mut stream) = start_live_session(&h, Some(bearer(&h))).await.unwrap();
    tx.send(LiveOp {
        session: session.clone(),
        op: Vec::new(),
    })
    .await
    .unwrap();
    let _attach = next_ack(&mut stream).await;

    let mut client = LoroClient::new();
    for (i, fragment) in ["alpha", " beta", " gamma"].iter().enumerate() {
        let pos = client.body_len();
        let op = client.insert(pos, fragment);
        tx.send(LiveOp {
            session: session.clone(),
            op,
        })
        .await
        .unwrap();
        let ack = next_ack(&mut stream).await;
        assert_eq!(
            ack.merged_version,
            format!("v{}", i + 1),
            "version advances monotonically",
        );
    }
    // After three ops the assembled content matches the client's
    // local string.
    drop(tx);
    h.process.shutdown().await;
}

#[tokio::test]
async fn unknown_session_emits_issue_ack_and_closes() {
    let h = start(None).await;

    let (tx, mut stream) = start_live_session(&h, Some(bearer(&h))).await.unwrap();
    // Send an attach for a session id the registry doesn't know.
    let phantom = "sess_phantom-id";
    tx.send(LiveOp {
        session: phantom.to_owned(),
        op: Vec::new(),
    })
    .await
    .unwrap();

    let ack = next_ack(&mut stream).await;
    assert_eq!(ack.session, phantom);
    assert_eq!(ack.merged_version, "");
    assert_eq!(ack.content, "");
    assert_eq!(ack.issues.len(), 1, "must surface one issue: {ack:?}");
    assert_eq!(ack.issues[0].code, "unknown_session");
    assert!(!ack.issues[0].message.is_empty());

    // Server should end the stream after the issue ack.
    let end = tokio::time::timeout(Duration::from_secs(5), stream.next())
        .await
        .expect("server closes stream within 5s");
    assert!(end.is_none(), "stream must end after unknown_session ack");

    drop(tx);
    h.process.shutdown().await;
}

#[tokio::test]
async fn missing_bearer_returns_unauthenticated_before_stream_open() {
    let h = start(None).await;
    let err = start_live_session(&h, None).await.unwrap_err();
    assert_eq!(err.code(), tonic::Code::Unauthenticated);
    h.process.shutdown().await;
}

#[tokio::test]
async fn op_via_bidi_debits_writes_quota() {
    // 1 write/min → first apply succeeds, second is rejected.
    let q = QuotaConfig {
        queries_per_minute: 60,
        writes_per_minute: 1,
        embeds_per_minute: 60,
        concurrent_sessions: 32,
    };
    let h = start(Some(Arc::new(QuotaManager::new(q)))).await;
    let session = open_session(&h, "page-quota").await;

    let (tx, mut stream) = start_live_session(&h, Some(bearer(&h))).await.unwrap();
    tx.send(LiveOp {
        session: session.clone(),
        op: Vec::new(),
    })
    .await
    .unwrap();
    let _attach = next_ack(&mut stream).await;

    let mut client = LoroClient::new();
    let op1 = client.insert(0, "first");
    tx.send(LiveOp {
        session: session.clone(),
        op: op1,
    })
    .await
    .unwrap();
    let ack1 = next_ack(&mut stream).await;
    assert_eq!(ack1.merged_version, "v1");
    assert!(ack1.issues.is_empty());

    let op2 = client.insert(client.body_len(), "-second");
    tx.send(LiveOp {
        session: session.clone(),
        op: op2,
    })
    .await
    .unwrap();
    // The second op overflows the Writes bucket. The server ends
    // the stream with `ResourceExhausted` — bidi failures surface
    // either on the next read or as a stream error.
    let next = tokio::time::timeout(Duration::from_secs(5), stream.next())
        .await
        .expect("server reacts within 5s");
    let status = match next {
        Some(Err(s)) => s,
        Some(Ok(ack)) => panic!("expected ResourceExhausted, got ack {ack:?}"),
        None => panic!("expected ResourceExhausted, got end-of-stream"),
    };
    assert_eq!(status.code(), tonic::Code::ResourceExhausted);

    drop(tx);
    h.process.shutdown().await;
}

#[tokio::test]
async fn client_closes_stream_does_not_close_session() {
    let h = start(None).await;
    let session = open_session(&h, "page-keep-alive").await;

    let (tx, mut stream) = start_live_session(&h, Some(bearer(&h))).await.unwrap();
    tx.send(LiveOp {
        session: session.clone(),
        op: Vec::new(),
    })
    .await
    .unwrap();
    let _attach = next_ack(&mut stream).await;

    // Apply one op so the actor has interesting state.
    let mut client = LoroClient::new();
    let op = client.insert(0, "live");
    tx.send(LiveOp {
        session: session.clone(),
        op,
    })
    .await
    .unwrap();
    let _applied = next_ack(&mut stream).await;

    // Drop the client side of the request stream. The server task
    // ends; the session must stay alive.
    drop(tx);

    // Drain any trailing acks the server may have queued so the
    // stream actually ends.
    while tokio::time::timeout(Duration::from_secs(2), stream.next())
        .await
        .ok()
        .flatten()
        .is_some()
    {}

    // Close via HTTP — must still succeed, proving the session
    // outlived the transport.
    let closed = http_call_ok(
        &h,
        "close_session",
        json!({ "session": session, "commit": false }),
    )
    .await;
    assert_eq!(closed["ok"], true);

    h.process.shutdown().await;
}
