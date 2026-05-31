//! End-to-end tests for the M4.2 live-CRDT MCP tools:
//! `open_session`, `apply_op`, `close_session`.
//!
//! Real running gateway, real `LiveDoc` actor over a real
//! `DuckdbCrdtBackend`. Op bytes are produced by a real Loro
//! `Client` peer (the same shape described in
//! `docs/notes/discovered/2026-05-25-loro-incremental-updates-need-persistent-client.md`)
//! and shuttled as base64 over JSON-RPC — no mocks at the
//! `LiveDoc`/backend boundary.
//!
//! These tests cover the three new tools end-to-end:
//! * happy path (open → apply → close) round-trips a Loro op blob;
//! * snapshot is persisted iff `commit=true`;
//! * `tools/list` advertises all three;
//! * malformed input maps to a JSON-RPC `-32602` invalid_params;
//! * an unknown session id maps to a JSON-RPC `-32603` internal;
//! * a server started with `crdt_backend=None` rejects with a
//!   JSON-RPC `-32603` internal explaining live mode is disabled;
//! * the per-tenant session semaphore caps concurrent
//!   `open_session` calls (no live verifier + a `QuotaConfig` with
//!   `concurrent_sessions=1`).

use std::sync::Arc;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use duckdb::Connection;
use escurel_crdt::{CrdtBackend, DuckdbCrdtBackend};
use escurel_index::Migrator;
use escurel_quota::{QuotaConfig, QuotaManager};
use escurel_test_support::{AuthMode, ConfigOverrides, EscurelProcess, Opts};
use loro::{ExportMode, LoroDoc};
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::sync::Mutex;

/// Persistent Loro client peer; mirrors `Client` in
/// `crates/escurel-crdt/tests/livedoc_roundtrip.rs`. Each
/// `insert` returns an *incremental* update anchored to the
/// previously exported frontier so the actor's doc can import it
/// cleanly.
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

    fn body_len(&self) -> usize {
        self.doc.get_text("body").len_unicode()
    }
}

struct Harness {
    process: EscurelProcess,
    http: reqwest::Client,
    /// Direct handle on the shared DuckDB connection, for asserting
    /// snapshot rows from the test side.
    conn: Arc<Mutex<Connection>>,
    _db_dir: TempDir,
}

/// Start a gateway with the given quota manager (or none) and an
/// optional live CRDT backend. When `with_crdt = true` the backend
/// is wired and the three session tools are functional; when
/// `false` the tools surface the `-32603` "live CRDT mode not
/// enabled" error.
///
/// The session tools route before the indexer gate in `mcp.rs`, so
/// this harness skips the indexer entirely — keeping the test boot
/// time low and avoiding the HNSW autoload gotcha that bites a
/// second connection opened on the same DuckDB file (see
/// `docs/notes/discovered/2026-05-24-duckdb-second-connection-stale.md`
/// and `…-duckdb-vss-fts-autoload.md`).
async fn start(quota: Option<Arc<QuotaManager>>, with_crdt: bool) -> Harness {
    let db_dir = TempDir::new().unwrap();
    let conn = Connection::open(db_dir.path().join("escurel.duckdb")).unwrap();
    Migrator::up(&conn).unwrap();
    let shared = Arc::new(Mutex::new(conn));

    let crdt_backend: Option<Arc<dyn CrdtBackend>> = if with_crdt {
        Some(Arc::new(DuckdbCrdtBackend::new(Arc::clone(&shared))))
    } else {
        None
    };

    let process = EscurelProcess::spawn(Opts {
        auth: AuthMode::Disabled,
        fixtures: None,
        config_overrides: ConfigOverrides {
            quota,
            crdt_backend,
            disable_indexer: true,
            ..Default::default()
        },
    })
    .await;
    Harness {
        process,
        http: reqwest::Client::new(),
        conn: shared,
        _db_dir: db_dir,
    }
}

/// Issue a JSON-RPC `tools/call` and return the raw response body.
async fn call_raw(h: &Harness, id: u64, name: &str, args: Value) -> Value {
    let resp = h
        .http
        .post(h.process.mcp_url())
        .json(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "tools/call",
            "params": { "name": name, "arguments": args },
        }))
        .send()
        .await
        .expect("post");
    assert_eq!(resp.status(), 200, "http status");
    resp.json().await.unwrap()
}

/// As above but assert the call succeeded, return `result`.
async fn call_ok(h: &Harness, id: u64, name: &str, args: Value) -> Value {
    let body = call_raw(h, id, name, args).await;
    assert!(body.get("error").is_none(), "{name} returned error: {body}");
    body["result"].clone()
}

#[tokio::test]
async fn open_session_returns_session_id_and_head_version() {
    let h = start(None, true).await;
    let result = call_ok(&h, 1, "open_session", json!({ "page_id": "page-a" })).await;
    let session = result["session"].as_str().expect("session id");
    assert!(
        session.starts_with("sess_"),
        "session id must be `sess_…`: {session}"
    );
    // Empty page → head is v0; ws_url must be present (advisory).
    assert_eq!(result["head_version"], "v0");
    assert!(
        result["ws_url"]
            .as_str()
            .map(|u| u.contains("/ws"))
            .unwrap_or(false),
        "ws_url should advertise /ws: {result}"
    );
    h.process.shutdown().await;
}

#[tokio::test]
async fn apply_op_to_open_session_updates_doc() {
    let h = start(None, true).await;
    let opened = call_ok(&h, 1, "open_session", json!({ "page_id": "page-apply" })).await;
    let session = opened["session"].as_str().unwrap().to_owned();

    let mut client = Client::new();
    let op_bytes = client.insert(0, "hello");
    let op_b64 = B64.encode(op_bytes);

    let result = call_ok(
        &h,
        2,
        "apply_op",
        json!({ "session": session, "op": op_b64 }),
    )
    .await;
    assert_eq!(result["ok"], true);
    assert_eq!(result["merged_version"], "v1");

    // A second op advances the version further.
    let op2 = client.insert(client.body_len(), " world");
    let result2 = call_ok(
        &h,
        3,
        "apply_op",
        json!({ "session": session, "op": B64.encode(op2) }),
    )
    .await;
    assert_eq!(result2["merged_version"], "v2");

    h.process.shutdown().await;
}

#[tokio::test]
async fn close_session_persists_snapshot() {
    let h = start(None, true).await;
    let opened = call_ok(&h, 1, "open_session", json!({ "page_id": "page-snap" })).await;
    let session = opened["session"].as_str().unwrap().to_owned();

    let mut client = Client::new();
    let op = client.insert(0, "snapshot-me");
    let _ = call_ok(
        &h,
        2,
        "apply_op",
        json!({ "session": session, "op": B64.encode(op) }),
    )
    .await;

    let closed = call_ok(
        &h,
        3,
        "close_session",
        json!({ "session": session, "commit": true }),
    )
    .await;
    assert_eq!(closed["ok"], true);
    assert_eq!(closed["final_version"], "v1");

    // Snapshot row must now exist for `page-snap`.
    let guard = h.conn.lock().await;
    let count: i64 = guard
        .query_row(
            "SELECT count(*) FROM crdt_snapshots WHERE page_id = ?",
            ["page-snap"],
            |row| row.get(0),
        )
        .unwrap();
    drop(guard);
    assert_eq!(count, 1, "snapshot must be persisted on commit=true");

    h.process.shutdown().await;
}

#[tokio::test]
async fn close_session_with_commit_false_does_not_snapshot() {
    let h = start(None, true).await;
    let opened = call_ok(&h, 1, "open_session", json!({ "page_id": "page-no-snap" })).await;
    let session = opened["session"].as_str().unwrap().to_owned();

    let mut client = Client::new();
    let op = client.insert(0, "do-not-persist");
    let _ = call_ok(
        &h,
        2,
        "apply_op",
        json!({ "session": session, "op": B64.encode(op) }),
    )
    .await;

    let closed = call_ok(
        &h,
        3,
        "close_session",
        json!({ "session": session, "commit": false }),
    )
    .await;
    assert_eq!(closed["ok"], true);

    let guard = h.conn.lock().await;
    let count: i64 = guard
        .query_row(
            "SELECT count(*) FROM crdt_snapshots WHERE page_id = ?",
            ["page-no-snap"],
            |row| row.get(0),
        )
        .unwrap();
    drop(guard);
    assert_eq!(count, 0, "snapshot must NOT be persisted on commit=false");

    h.process.shutdown().await;
}

#[tokio::test]
async fn open_session_without_crdt_backend_returns_jsonrpc_error() {
    let h = start(None, false).await;
    let body = call_raw(&h, 1, "open_session", json!({ "page_id": "any" })).await;
    let err = body.get("error").expect("error envelope");
    assert_eq!(err["code"], -32603, "internal: {body}");
    let msg = err["message"].as_str().unwrap_or("");
    assert!(
        msg.contains("live CRDT"),
        "message should mention live CRDT mode: {msg}"
    );
    h.process.shutdown().await;
}

#[tokio::test]
async fn apply_op_with_unknown_session_returns_jsonrpc_error() {
    let h = start(None, true).await;
    let body = call_raw(
        &h,
        1,
        "apply_op",
        json!({ "session": "sess_does-not-exist", "op": B64.encode(b"x") }),
    )
    .await;
    let err = body.get("error").expect("error envelope");
    assert_eq!(err["code"], -32603, "internal: {body}");
    h.process.shutdown().await;
}

#[tokio::test]
async fn apply_op_with_malformed_base64_returns_invalid_params() {
    let h = start(None, true).await;
    let opened = call_ok(&h, 1, "open_session", json!({ "page_id": "page-bad-b64" })).await;
    let session = opened["session"].as_str().unwrap().to_owned();

    let body = call_raw(
        &h,
        2,
        "apply_op",
        json!({ "session": session, "op": "!!!not-base64!!!" }),
    )
    .await;
    let err = body.get("error").expect("error envelope");
    assert_eq!(err["code"], -32602, "invalid_params: {body}");
    h.process.shutdown().await;
}

#[tokio::test]
async fn tools_list_now_advertises_open_apply_close() {
    let h = start(None, true).await;
    let resp = h
        .http
        .post(h.process.mcp_url())
        .json(&json!({
            "jsonrpc": "2.0",
            "id": 100,
            "method": "tools/list",
        }))
        .send()
        .await
        .unwrap();
    let body: Value = resp.json().await.unwrap();
    let tools = body["result"]["tools"].as_array().expect("tools array");
    let names: Vec<&str> = tools.iter().filter_map(|t| t["name"].as_str()).collect();
    for name in ["open_session", "apply_op", "close_session"] {
        assert!(
            names.contains(&name),
            "tools/list missing {name}: {names:?}"
        );
    }
    h.process.shutdown().await;
}

#[tokio::test]
async fn quota_caps_concurrent_sessions() {
    // The per-tenant semaphore caps concurrent open sessions at the
    // configured value. Without auth wired the tenant id used by
    // the SessionManager defaults to a sentinel; either way, the
    // semaphore must refuse the second open while the first is
    // still alive, and accept it once the first closes.
    let q = QuotaConfig {
        queries_per_minute: 600,
        writes_per_minute: 120,
        embeds_per_minute: 300,
        concurrent_sessions: 1,
    };
    let h = start(Some(Arc::new(QuotaManager::new(q))), true).await;

    let first = call_ok(&h, 1, "open_session", json!({ "page_id": "page-a" })).await;
    let session_a = first["session"].as_str().unwrap().to_owned();

    let denied = call_raw(&h, 2, "open_session", json!({ "page_id": "page-b" })).await;
    // We accept either the JSON-RPC error envelope or an HTTP 429
    // — but the call_raw helper asserts 200, so it must be the
    // JSON-RPC envelope path.
    let err = denied
        .get("error")
        .unwrap_or_else(|| panic!("expected quota error, got {denied}"));
    let code = err["code"].as_i64().unwrap_or(0);
    assert!(
        code == -32000 || code == -32603,
        "quota refusal should be -32000 (quota) or -32603 (internal): {denied}"
    );

    // After the first session closes, a fresh open succeeds.
    let _ = call_ok(
        &h,
        3,
        "close_session",
        json!({ "session": session_a, "commit": false }),
    )
    .await;
    let third = call_ok(&h, 4, "open_session", json!({ "page_id": "page-c" })).await;
    assert!(third["session"].as_str().unwrap().starts_with("sess_"));

    h.process.shutdown().await;
}
