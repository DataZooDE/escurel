//! Attaching to a live session must respect the page's ACL.
//!
//! `/ws` gates the *upgrade* on auth + quota, and then `session_loop` accepts
//! any `hello` whose session id is open — `page_id_of(...).is_some()` is the
//! only membership probe. `ws.rs` contains no ACL reference at all.
//!
//! That matters because attaching is a **read**. `op_ack` carries the
//! session's current `content`, so a principal who may not read the page can
//! watch someone else's live editing by attaching to their session.
//!
//! Note precisely what does and does not leak, because the first version of
//! this file overstated it. `open_session` does NOT seed the doc from the
//! stored markdown — `LiveDoc` hydrates from CRDT history alone, so a page
//! never live-edited yields an empty doc and the stored body is not exposed
//! this way. What leaks is the **session's live content**: every keystroke
//! Alice has typed in that session, which is precisely the material she is
//! editing on a page the ACL says Bob may not read. The HTTP read path
//! enforces `may_read_instance` on that page; the WebSocket path did not, so
//! the session was a side channel around the instance ACL.
//!
//! Issue #352 names this property as acceptance for the broadcast work
//! ("a session must not become a side channel around group ACLs"). It has to
//! be true *before* ops fan out to other peers, since broadcasting widens the
//! same hole from "attach and ask" to "attach and be told".
//!
//! Real gateway, real `SessionManager` + `LiveDoc` over a real
//! `DuckdbCrdtBackend`, real DuckDB indexer, real OIDC, real
//! `tokio-tungstenite`. No mocks.

use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use duckdb::Connection;
use escurel_crdt::{CrdtBackend, DuckdbCrdtBackend};
use escurel_index::Migrator;
use escurel_test_support::{AuthMode, ConfigOverrides, EscurelProcess, FixtureBuilder, Opts, Role};
use futures::{SinkExt, StreamExt};
use loro::{ExportMode, LoroDoc};
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::sync::Mutex;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::handshake::client::Request as WsRequest;
use tokio_tungstenite::tungstenite::protocol::Message;

const TENANT: &str = "stuttgart-ai";
const ALICE: &str = "whatsapp:111";
const BOB: &str = "whatsapp:222";

const MEMBER_SKILL: &str = "---\ntype: skill\nid: community_member\n\
    description: A member.\nvisibility: owner\nowner_field: credential\n---\n# community_member\n";
const ALICE_MEMBER: &str = "---\ntype: instance\nskill: community_member\nid: alice\n\
    credential: \"whatsapp:111\"\n---\n# Alice\n\nSECRET-DIARY-LINE.\n";
const BOB_MEMBER: &str = "---\ntype: instance\nskill: community_member\nid: bob\n\
    credential: \"whatsapp:222\"\n---\n# Bob\n";

/// Alice's own page — owner-private, so Bob may not read it.
const ALICE_PAGE: &str = "markdown/instances/community_member/alice.md";

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

async fn start() -> Harness {
    let db_dir = TempDir::new().unwrap();
    let conn = Connection::open(db_dir.path().join("crdt.duckdb")).unwrap();
    Migrator::up(&conn).unwrap();
    let shared = Arc::new(Mutex::new(conn));
    let crdt_backend: Arc<dyn CrdtBackend> = Arc::new(DuckdbCrdtBackend::new(Arc::clone(&shared)));

    let process = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(
            FixtureBuilder::new()
                .tenant(TENANT)
                .skill("community_member", MEMBER_SKILL)
                .instance("community_member", "alice", ALICE_MEMBER)
                .instance("community_member", "bob", BOB_MEMBER)
                .done(),
        ),
        config_overrides: ConfigOverrides {
            crdt_backend: Some(crdt_backend),
            ..Default::default()
        },
    })
    .await;
    Harness {
        process,
        _db_dir: db_dir,
    }
}

fn ws_request(url: &str, bearer: &str) -> WsRequest {
    let mut req = url.into_client_request().unwrap();
    req.headers_mut()
        .insert("authorization", format!("Bearer {bearer}").parse().unwrap());
    req
}

type Sock =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

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

async fn send_json(sock: &mut Sock, v: Value) {
    sock.send(Message::Text(v.to_string())).await.unwrap();
}

async fn call(h: &Harness, bearer: &str, name: &str, args: Value) -> Value {
    reqwest::Client::new()
        .post(h.process.mcp_url())
        .bearer_auth(bearer)
        .json(&json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": { "name": name, "arguments": args } }))
        .send()
        .await
        .expect("post")
        .json()
        .await
        .expect("json")
}

/// Bob must not be able to attach to a session on Alice's owner-private page.
///
/// The positive control comes first: Alice's own attach succeeds, so a
/// rejection for Bob cannot be an artefact of the session being unusable.
#[tokio::test]
async fn a_principal_who_may_not_read_the_page_cannot_attach_to_its_session() {
    let h = start().await;
    let alice = h.process.mint_token_with_sub(TENANT, Role::Agent, ALICE);
    let bob = h.process.mint_token_with_sub(TENANT, Role::Agent, BOB);

    // Alice opens a live session on her own page.
    let opened = call(&h, &alice, "open_session", json!({ "page_id": ALICE_PAGE })).await;
    let session = opened["result"]["structuredContent"]["session"]
        .as_str()
        .expect("session id")
        .to_owned();

    let url = h.process.ws_url();

    // Positive control: Alice attaches and is accepted.
    {
        let (mut sock, _) = tokio_tungstenite::connect_async(ws_request(&url, &alice))
            .await
            .expect("alice ws connect");
        send_json(&mut sock, json!({ "type": "hello", "session": session })).await;
        send_json(&mut sock, json!({ "type": "presence", "cursor": 0 })).await;
        let echoed = recv_json(&mut sock).await;
        assert_eq!(
            echoed["type"], "presence",
            "control: the owner's attach must work, else the negative case \
             below proves nothing: {echoed}"
        );
    }

    // Bob attaches to the same session. He may not read Alice's page.
    let (mut sock, _) = tokio_tungstenite::connect_async(ws_request(&url, &bob))
        .await
        .expect("bob ws connect");
    send_json(&mut sock, json!({ "type": "hello", "session": session })).await;
    send_json(&mut sock, json!({ "type": "presence", "cursor": 0 })).await;

    let reply = recv_json(&mut sock).await;
    assert_eq!(
        reply["type"], "error",
        "Bob may not read Alice's owner-private page, so attaching to a \
         session on it must be refused — attaching is a read: {reply}"
    );
    assert_eq!(
        reply["code"], "forbidden",
        "the refusal must be typed so a client can tell it from a transport \
         fault: {reply}"
    );
}

/// The leak this closes, asserted on the bytes that actually move.
///
/// Alice types into her session; Bob attaches and reads `op_ack.content`.
/// Without the gate he receives her live text verbatim. Uses a real Loro op
/// so the content is genuinely the session's, not a fixture artefact.
#[tokio::test]
async fn an_unauthorised_attach_never_yields_the_session_content() {
    let h = start().await;
    let alice = h.process.mint_token_with_sub(TENANT, Role::Agent, ALICE);
    let bob = h.process.mint_token_with_sub(TENANT, Role::Agent, BOB);

    // Precondition: the HTTP read path already denies Bob this page, so the
    // WebSocket is the only boundary under test.
    let expanded = call(&h, &bob, "expand", json!({ "page_id": ALICE_PAGE })).await;
    assert!(
        !serde_json::to_string(&expanded)
            .unwrap_or_default()
            .contains("SECRET-DIARY-LINE"),
        "precondition: HTTP `expand` must already deny Bob: {expanded}"
    );

    let opened = call(&h, &alice, "open_session", json!({ "page_id": ALICE_PAGE })).await;
    let session = opened["result"]["structuredContent"]["session"]
        .as_str()
        .expect("session id")
        .to_owned();

    // Alice types. A persistent Loro peer per
    // `docs/notes/discovered/2026-05-25-loro-incremental-updates-need-persistent-client.md`.
    let mut peer = Client::new();
    let op = B64.encode(peer.insert(0, "ALICE-LIVE-DRAFT"));
    let applied = call(
        &h,
        &alice,
        "apply_op",
        json!({ "session": session, "op": op }),
    )
    .await;
    assert!(
        applied.get("error").is_none(),
        "alice's op must apply, else there is nothing to leak: {applied}"
    );

    // Bob attaches and asks for the session state.
    let url = h.process.ws_url();
    let (mut sock, _) = tokio_tungstenite::connect_async(ws_request(&url, &bob))
        .await
        .expect("bob ws connect");
    send_json(&mut sock, json!({ "type": "hello", "session": session })).await;
    let mut peer_b = Client::new();
    let bob_op = B64.encode(peer_b.insert(0, "x"));
    send_json(
        &mut sock,
        json!({ "type": "op", "session": session, "op": bob_op }),
    )
    .await;

    let reply = recv_json(&mut sock).await;
    let seen = serde_json::to_string(&reply).unwrap_or_default();
    assert!(
        !seen.contains("ALICE-LIVE-DRAFT"),
        "Bob must not receive Alice's live session text from a page he may \
         not read: {reply}"
    );
}
