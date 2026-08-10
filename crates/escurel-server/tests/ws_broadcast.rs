//! Ops and presence reach the *other* peers attached to a session (#352).
//!
//! Before this, a session was single-peer: `handle_op` replied `op_ack` to the
//! originating socket and nothing else, and the server kept no registry of
//! sockets per session, so no frame type had a path to a second peer. Two
//! devices on one session stayed silently divergent — each learned of the
//! other's edits only by asking again.
//!
//! The CRDT layer was never the problem: ops merged and persisted correctly.
//! The gap was purely transport fan-out, which is why the fix is a dispatcher
//! and not a change to `LiveDoc`.
//!
//! Real gateway, real `SessionManager` + `LiveDoc` over a real
//! `DuckdbCrdtBackend`, real OIDC, two real `tokio-tungstenite` clients.
//! No mocks.

use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use duckdb::Connection;
use escurel_crdt::{CrdtBackend, DuckdbCrdtBackend};
use escurel_index::Migrator;
use escurel_test_support::{AuthMode, ConfigOverrides, EscurelProcess, Opts, Role};
use futures::{SinkExt, StreamExt};
use loro::{ExportMode, LoroDoc};
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::sync::Mutex;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::handshake::client::Request as WsRequest;
use tokio_tungstenite::tungstenite::protocol::Message;

const TENANT: &str = "acme";
const PAGE: &str = "markdown/instances/note/n1.md";

/// A persistent Loro peer — incremental updates require one, per
/// `docs/notes/discovered/2026-05-25-loro-incremental-updates-need-persistent-client.md`.
struct Client {
    doc: LoroDoc,
    vv: loro::VersionVector,
}

impl Client {
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
}

struct Harness {
    process: EscurelProcess,
    _db_dir: TempDir,
}

/// Session-only gateway (no indexer): this file is about transport fan-out,
/// and the ACL half is covered by `ws_attach_acl.rs`.
async fn start() -> Harness {
    let db_dir = TempDir::new().unwrap();
    let conn = Connection::open(db_dir.path().join("crdt.duckdb")).unwrap();
    Migrator::up(&conn).unwrap();
    let shared = Arc::new(Mutex::new(conn));
    let crdt_backend: Arc<dyn CrdtBackend> = Arc::new(DuckdbCrdtBackend::new(Arc::clone(&shared)));

    let process = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: None,
        config_overrides: ConfigOverrides {
            crdt_backend: Some(crdt_backend),
            disable_indexer: true,
            ..Default::default()
        },
    })
    .await;
    Harness {
        process,
        _db_dir: db_dir,
    }
}

type Sock =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

fn ws_request(url: &str, bearer: &str) -> WsRequest {
    let mut req = url.into_client_request().unwrap();
    req.headers_mut()
        .insert("authorization", format!("Bearer {bearer}").parse().unwrap());
    req
}

async fn recv_json(sock: &mut Sock) -> Value {
    let msg = tokio::time::timeout(Duration::from_secs(3), sock.next())
        .await
        .expect("recv timed out")
        .expect("stream ended")
        .expect("ws error");
    let txt = match msg {
        Message::Text(t) => t,
        Message::Binary(b) => String::from_utf8(b).unwrap(),
        other => panic!("expected text frame, got {other:?}"),
    };
    serde_json::from_str(&txt).unwrap()
}

/// Receive frames until one satisfies `want`, or time out.
///
/// A peer's stream carries whatever the server chooses to send; asserting on
/// "the next frame" would couple the test to ordering it does not care about.
async fn recv_until(sock: &mut Sock, want: impl Fn(&Value) -> bool) -> Option<Value> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return None;
        }
        match tokio::time::timeout(remaining, sock.next()).await {
            Ok(Some(Ok(Message::Text(t)))) => {
                if let Ok(v) = serde_json::from_str::<Value>(&t)
                    && want(&v)
                {
                    return Some(v);
                }
            }
            Ok(Some(Ok(_))) => {}
            _ => return None,
        }
    }
}

async fn send_json(sock: &mut Sock, v: Value) {
    sock.send(Message::Text(v.to_string())).await.unwrap();
}

async fn open_session(h: &Harness, bearer: &str) -> String {
    let resp: Value = reqwest::Client::new()
        .post(h.process.mcp_url())
        .bearer_auth(bearer)
        .json(&json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": { "name": "open_session", "arguments": { "page_id": PAGE } } }))
        .send()
        .await
        .expect("post")
        .json()
        .await
        .expect("json");
    resp["result"]["structuredContent"]["session"]
        .as_str()
        .expect("session id")
        .to_owned()
}

/// Attach a socket to `session` and return it.
async fn attach(h: &Harness, bearer: &str, session: &str) -> Sock {
    let (mut sock, _) = tokio_tungstenite::connect_async(ws_request(&h.process.ws_url(), bearer))
        .await
        .expect("ws connect");
    send_json(&mut sock, json!({ "type": "hello", "session": session })).await;
    sock
}

/// An op applied by one attached client reaches the other.
///
/// This is the headline acceptance criterion of #352.
#[tokio::test]
async fn an_op_from_one_peer_is_delivered_to_the_other() {
    let h = start().await;
    let token = h.process.mint_token(TENANT, Role::Agent);
    let session = open_session(&h, &token).await;

    let mut a = attach(&h, &token, &session).await;
    let mut b = attach(&h, &token, &session).await;

    let mut peer = Client::new();
    let op = B64.encode(peer.insert(0, "HELLO-FROM-A"));
    send_json(
        &mut a,
        json!({ "type": "op", "session": session, "op": op }),
    )
    .await;

    // Positive control: A's own ack arrives, so a silent B cannot be an
    // artefact of the op never being processed.
    let ack = recv_until(&mut a, |v| v["type"] == "op_ack").await;
    assert!(ack.is_some(), "control: A must receive its own op_ack");

    let peer_frame = recv_until(&mut b, |v| v["type"] == "peer_op").await;
    let frame = peer_frame.expect(
        "B received nothing: an op applied by one attached client must reach \
         the others on the same session (#352)",
    );
    assert_eq!(frame["session"], session.as_str(), "frame: {frame}");
    assert!(
        frame["content"]
            .as_str()
            .unwrap_or_default()
            .contains("HELLO-FROM-A"),
        "the delivered frame must carry the merged content so B can render \
         without a round trip: {frame}"
    );
}

/// The sender does not receive its own broadcast.
///
/// A already gets `op_ack`; echoing the same edit back as a peer frame would
/// make every client apply its own op twice.
#[tokio::test]
async fn a_peer_does_not_receive_its_own_op_back() {
    let h = start().await;
    let token = h.process.mint_token(TENANT, Role::Agent);
    let session = open_session(&h, &token).await;

    let mut a = attach(&h, &token, &session).await;
    let _b = attach(&h, &token, &session).await;

    let mut peer = Client::new();
    let op = B64.encode(peer.insert(0, "ONLY-ONCE"));
    send_json(
        &mut a,
        json!({ "type": "op", "session": session, "op": op }),
    )
    .await;

    let ack = recv_until(&mut a, |v| v["type"] == "op_ack").await;
    assert!(ack.is_some(), "A must receive its own op_ack");

    let echoed = recv_until(&mut a, |v| v["type"] == "peer_op").await;
    assert!(
        echoed.is_none(),
        "the originator must not be sent its own edit as a peer frame: {echoed:?}"
    );
}

/// Presence reaches other peers — the "live cursors" half of #352.
#[tokio::test]
async fn presence_reaches_the_other_peer() {
    let h = start().await;
    let token = h.process.mint_token(TENANT, Role::Agent);
    let session = open_session(&h, &token).await;

    let mut a = attach(&h, &token, &session).await;
    let mut b = attach(&h, &token, &session).await;

    send_json(
        &mut a,
        json!({ "type": "presence", "session": session, "cursor": 42 }),
    )
    .await;

    // A still gets its own echo (unchanged behaviour, relied on by clients
    // to confirm round-trip).
    let own = recv_until(&mut a, |v| v["type"] == "presence").await;
    assert!(own.is_some(), "control: A's presence echo is unchanged");

    let seen = recv_until(&mut b, |v| v["type"] == "presence").await;
    let frame = seen.expect("B must receive A's presence frame — live cursors need it");
    assert_eq!(frame["cursor"], 42, "cursor must survive the hop: {frame}");
}

/// A session with one peer still works, and nothing is broadcast into the void.
#[tokio::test]
async fn a_single_peer_session_behaves_as_before() {
    let h = start().await;
    let token = h.process.mint_token(TENANT, Role::Agent);
    let session = open_session(&h, &token).await;

    let mut a = attach(&h, &token, &session).await;
    let mut peer = Client::new();
    let op = B64.encode(peer.insert(0, "SOLO"));
    send_json(
        &mut a,
        json!({ "type": "op", "session": session, "op": op }),
    )
    .await;

    let ack = recv_json(&mut a).await;
    assert_eq!(ack["type"], "op_ack", "single-peer op_ack unchanged: {ack}");
    assert!(
        ack["content"].as_str().unwrap_or_default().contains("SOLO"),
        "content still returned to the sole peer: {ack}"
    );
}
