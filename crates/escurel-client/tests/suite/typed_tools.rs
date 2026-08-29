//! Typed shapes for the advertised agent-scope tools that previously had
//! no request/response structs or client methods (`fetch_blob`,
//! `list_snapshots`, `list_op_authors`, `write_instance`, and the session
//! trio `open_session`/`apply_op`/`close_session`), plus `error.data`
//! (`{code, retryable}`) surfacing through the typed error.
//!
//! Real gateway via `escurel-test-support` (real DuckDB, real MCP-over-HTTP,
//! real OIDC test issuer, real DuckDB CRDT backend for the session paths,
//! and a real stateful loopback upstream for `write_instance`) — no mocks
//! at the boundary (CLAUDE principle 2).

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use axum::extract::{Path, State};
use axum::routing::get;
use axum::{Json, Router};
use base64::Engine as _;
use duckdb::Connection;
use escurel_client::{
    ApplyOpRequest, Client, CloseSessionRequest, Error, ExpandRequest, FetchBlobRequest,
    ListOpAuthorsRequest, ListSnapshotsRequest, OpenSessionRequest, SecretString,
    UpdatePageRequest, WriteInstanceRequest,
};
use escurel_crdt::{CrdtBackend, DuckdbCrdtBackend};
use escurel_embed::{Embedder, ZeroEmbedder};
use escurel_index::{Indexer, Migrator};
use escurel_storage::{FsStore, LaneStore};
use escurel_test_support::crdt_testkit::loro_insert_op;
use escurel_test_support::{AuthMode, ConfigOverrides, EscurelProcess, FixtureBuilder, Opts, Role};
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::net::TcpListener;

const TENANT: &str = "acme";

const CUSTOMER_SKILL: &str = r"---
type: skill
id: customer
description: A buying organisation.
required_frontmatter: [id, name]
---
# customer
";

const ACME_PAGE_ID: &str = "markdown/instances/customer/acme.md";
const ACME_INSTANCE: &str = r"---
type: instance
skill: customer
id: acme
name: Acme Corp
---
# Acme Corp
";

/// A `document`-backend skill so `/ingest/upload` + `fetch_blob` have a
/// real blob-retaining pipeline to run through (born-digital text via the
/// PlainTextExtractor — fully offline). Raw string: the YAML indentation
/// of the `backend:` block is load-bearing.
const MEMO_SKILL: &str = r"---
type: skill
id: memo
description: Text memos ingested as documents.
backend:
  kind: document
  accepts: [text/plain]
  chunk: { max_chars: 40, overlap: 8 }
---
# memo
";

/// An `openapi` remote-proxy skill whose write op PATCHes the upstream.
const REMOTE_CUSTOMER_SKILL: &str = r#"---
type: skill
id: customer
description: CRM customers, proxied live over REST.
backend:
  kind: openapi
  endpoint: crm_rest
  read: { path: "/customers/{id}" }
  write: { method: PATCH, path: "/customers/{id}" }
  project: { display_name: $.name, tier: $.account_tier }
---
# customer
"#;

/// Gateway whose CRDT backend shares the INDEXER's DuckDB instance via
/// `try_clone` — the production single-file wiring (a second
/// `Connection::open` would be a separate instance), and the shape where
/// `list_snapshots` (read through the indexer's connection) sees the
/// snapshots the write path takes.
struct LiveHarness {
    process: EscurelProcess,
    _dirs: Vec<TempDir>,
}

async fn start_live() -> LiveHarness {
    let store_dir = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    let store: Arc<dyn LaneStore> = Arc::new(FsStore::new(store_dir.path().to_path_buf()));
    let embedder: Arc<dyn Embedder> = Arc::new(ZeroEmbedder::default());
    let conn = Connection::open(db_dir.path().join("escurel.duckdb")).unwrap();
    Migrator::up(&conn).unwrap();
    // Second connection to the SAME instance, cloned before the write
    // connection moves into the indexer (mirrors SingleFileStore::open).
    let crdt_conn = conn.try_clone().unwrap();
    let indexer = Arc::new(Indexer::new(store, embedder, conn, TENANT).unwrap());
    indexer
        .update_page("markdown/skills/customer.md", CUSTOMER_SKILL)
        .await
        .unwrap();
    indexer
        .update_page(ACME_PAGE_ID, ACME_INSTANCE)
        .await
        .unwrap();
    let backend: Arc<dyn CrdtBackend> = Arc::new(DuckdbCrdtBackend::new(Arc::new(
        tokio::sync::Mutex::new(crdt_conn),
    )));
    let process = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: None,
        config_overrides: ConfigOverrides {
            indexer: Some(indexer),
            crdt_backend: Some(backend),
            ..Default::default()
        },
    })
    .await;
    LiveHarness {
        process,
        _dirs: vec![store_dir, db_dir],
    }
}

async fn client_as(p: &EscurelProcess, role: Role) -> Client {
    let token = p.mint_token(TENANT, role);
    Client::connect(p.base_url(), SecretString::from(token))
        .await
        .unwrap()
}

// ── the session trio, typed ───────────────────────────────────────

/// open_session → apply_op → list_op_authors → close_session through the
/// typed client, against the real DuckDB CRDT backend; the commit writes
/// through to the indexer so `expand` observes the merged body.
#[tokio::test]
async fn session_trio_round_trips_typed() {
    let h = start_live().await;
    let client = client_as(&h.process, Role::Agent).await;
    // Author a FRESH page through the session: the op carries the whole
    // markdown document, so the commit write-through parses cleanly.
    let page_id = "markdown/instances/customer/globex.md";
    let doc = "---\ntype: instance\nskill: customer\nid: globex\nname: Globex\n---\n\
               # Globex\n\ntyped-session-edit\n";

    let opened = client
        .open_session(OpenSessionRequest {
            page_id: page_id.to_owned(),
        })
        .await
        .unwrap();
    assert!(!opened.session.is_empty(), "session id minted");
    assert!(
        opened.head_version.starts_with('v'),
        "monotonic head version: {}",
        opened.head_version
    );
    assert_eq!(opened.ws_url, "/ws", "canonical relative WS path");

    let acked = client
        .apply_op(ApplyOpRequest {
            session: opened.session.clone(),
            op: loro_insert_op(doc),
        })
        .await
        .unwrap();
    assert!(acked.ok, "op applied");
    assert!(
        acked.merged_version.starts_with('v'),
        "merged version: {}",
        acked.merged_version
    );

    // The audit read: the op is attributed to the VERIFIED principal.
    let authors = client
        .list_op_authors(ListOpAuthorsRequest {
            page_id: page_id.to_owned(),
        })
        .await
        .unwrap();
    assert_eq!(authors.page_id, page_id);
    assert!(!authors.ops.is_empty(), "the applied op is listed");
    let op = &authors.ops[0];
    assert!(!op.op_id.is_empty());
    assert!(op.hlc >= 1, "hlc allocated: {}", op.hlc);
    assert!(
        op.principal.is_some(),
        "op attributed to the verified token subject"
    );

    let closed = client
        .close_session(CloseSessionRequest {
            session: opened.session,
            commit: true,
        })
        .await
        .unwrap();
    assert!(closed.ok);
    assert!(closed.final_version.starts_with('v'));

    // Commit write-through: the merged body is what `expand` now serves.
    let read = client
        .expand(ExpandRequest {
            page_id: page_id.to_owned(),
            ..Default::default()
        })
        .await
        .unwrap();
    assert!(
        read.body.contains("typed-session-edit"),
        "committed session edit reached the indexer: {}",
        read.body
    );
    h.process.shutdown().await;
}

/// `CloseSessionRequest::default()` must carry the wire default
/// `commit: true` — a defaulted close is a commit, not a silent discard.
#[test]
fn close_session_defaults_to_commit() {
    assert!(CloseSessionRequest::default().commit);
}

// ── list_snapshots, typed ─────────────────────────────────────────

/// Every whole-page write on a live-CRDT gateway snapshots; the typed
/// `list_snapshots` lists the taken_at history oldest-first.
#[tokio::test]
async fn list_snapshots_round_trips_typed() {
    let h = start_live().await;
    let client = client_as(&h.process, Role::Agent).await;

    let w = client
        .update_page(UpdatePageRequest {
            page_id: ACME_PAGE_ID.to_owned(),
            content: "---\ntype: instance\nskill: customer\nid: acme\nname: Acme Corp\n---\n# v2\n"
                .to_owned(),
            ..Default::default()
        })
        .await
        .unwrap();
    assert!(w.ok, "write lands: {:?}", w.issues);

    let snaps = client
        .list_snapshots(ListSnapshotsRequest {
            page_id: ACME_PAGE_ID.to_owned(),
        })
        .await
        .unwrap();
    assert!(
        !snaps.snapshots.is_empty(),
        "the write's snapshot is listed"
    );
    h.process.shutdown().await;
}

// ── fetch_blob, typed ─────────────────────────────────────────────

/// Upload a born-digital document through `/ingest/upload`, then fetch the
/// retained original bytes back through the typed `fetch_blob` — verbatim,
/// with the declared content type. A non-document page reads as a null
/// blob (`None`), indistinguishable from an absent one.
#[tokio::test]
async fn fetch_blob_round_trips_typed() {
    let p = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(
            FixtureBuilder::new()
                .tenant(TENANT)
                .skill("memo", MEMO_SKILL)
                .done(),
        ),
        config_overrides: ConfigOverrides::default(),
    })
    .await;
    let client = client_as(&p, Role::Agent).await;

    let body = "The original bytes of the source document, verbatim.";
    let ingested = client
        .ingest_upload("text/plain", body.as_bytes(), None, None)
        .await
        .unwrap();
    let page_id = ingested["page_id"]
        .as_str()
        .unwrap_or_else(|| panic!("ingest outcome carries page_id: {ingested}"))
        .to_owned();

    let fetched = client
        .fetch_blob(FetchBlobRequest {
            page_id: page_id.clone(),
        })
        .await
        .unwrap();
    let blob = fetched.blob.expect("document instance has a blob");
    assert_eq!(blob.page_id, page_id);
    assert_eq!(blob.content_type, "text/plain");
    assert_eq!(blob.size, body.len() as u64);
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(&blob.bytes_base64)
        .unwrap();
    assert_eq!(decoded, body.as_bytes(), "bytes round-trip verbatim");

    // The skill page (catalogue, not a document) → null blob, decoded None.
    let none = client
        .fetch_blob(FetchBlobRequest {
            page_id: "markdown/skills/memo.md".to_owned(),
        })
        .await
        .unwrap();
    assert!(none.blob.is_none(), "non-document page has no blob");
    p.shutdown().await;
}

// ── write_instance, typed ─────────────────────────────────────────

/// id → customer row; a PATCH mutates it so the next GET observes it.
type Crm = Arc<Mutex<std::collections::BTreeMap<String, Value>>>;

async fn get_customer(Path(id): Path<String>, State(db): State<Crm>) -> Json<Value> {
    let row = db.lock().unwrap().get(&id).cloned().unwrap_or(Value::Null);
    Json(row)
}

async fn patch_customer(
    Path(id): Path<String>,
    State(db): State<Crm>,
    Json(body): Json<Value>,
) -> Json<Value> {
    let mut guard = db.lock().unwrap();
    let row = guard.entry(id).or_insert_with(|| json!({}));
    if let (Some(obj), Some(patch)) = (row.as_object_mut(), body.as_object()) {
        for (k, v) in patch {
            obj.insert(k.clone(), v.clone());
        }
    }
    Json(row.clone())
}

/// A real, stateful loopback CRM (not a double of escurel code): the
/// boundary under test is escurel's proxy machinery + the typed client.
async fn start_crm() -> String {
    let db: Crm = Arc::new(Mutex::new(
        [(
            "acme".to_owned(),
            json!({ "name": "Acme Corp", "account_tier": "gold" }),
        )]
        .into_iter()
        .collect(),
    ));
    let app = Router::new()
        .route("/customers/{id}", get(get_customer).patch(patch_customer))
        .with_state(db);
    let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{addr}")
}

/// Register the endpoint + materialise the overlay (admin plumbing, via
/// `call_raw` — those admin tools are deliberately outside the typed
/// agent surface), then write through the typed `write_instance` and
/// verify the upstream really changed via a live read-after-write.
#[tokio::test]
async fn write_instance_round_trips_typed() {
    let crm_base = start_crm().await;
    let p = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(
            FixtureBuilder::new()
                .tenant(TENANT)
                .skill("customer", REMOTE_CUSTOMER_SKILL)
                .done(),
        ),
        config_overrides: ConfigOverrides::default(),
    })
    .await;
    let admin = client_as(&p, Role::Admin).await;

    let reg = admin
        .call_raw(
            "register_endpoint",
            json!({ "name": "crm_rest", "kind": "openapi", "base_url": crm_base }),
        )
        .await
        .unwrap();
    assert_eq!(reg["ok"], true, "register_endpoint: {reg}");
    let created = admin
        .call_raw(
            "create_remote_instance",
            json!({ "skill": "customer", "id": "acme" }),
        )
        .await
        .unwrap();
    let page_id = created["page_id"].as_str().expect("page_id").to_owned();

    let written = admin
        .write_instance(WriteInstanceRequest {
            instance_ref: "customer::acme".to_owned(),
            payload: json!({ "account_tier": "platinum" }),
        })
        .await
        .unwrap();
    assert!(written.ok, "write forwarded upstream");
    assert_eq!(written.source, "crm_rest");
    assert_eq!(
        written.fields["tier"],
        json!("platinum"),
        "re-projected write echo"
    );

    // Read-after-write through the live projection: the upstream state
    // genuinely changed (nothing canned).
    let read = admin
        .expand(ExpandRequest {
            page_id,
            ..Default::default()
        })
        .await
        .unwrap();
    let proj = read.backend_projection.expect("live projection present");
    assert_eq!(proj["fields"]["tier"], json!("platinum"), "{proj}");
    p.shutdown().await;
}

// ── error.data through the typed boundary ─────────────────────────

/// The gateway's typed refusals carry `error.data = {code, retryable}`;
/// docs tell callers to branch on them. The transport must surface both
/// through `Error::JsonRpc` instead of dropping them on the floor.
#[tokio::test]
async fn error_data_code_and_retryable_surface_typed() {
    // A ducklake reader replica rejects the whole mutating surface with
    // `{code: "read_only_replica", retryable: true}`.
    let p = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(
            FixtureBuilder::new()
                .tenant(TENANT)
                .skill("customer", CUSTOMER_SKILL)
                .done(),
        ),
        config_overrides: ConfigOverrides {
            reader_mode: true,
            ..Default::default()
        },
    })
    .await;
    let client = client_as(&p, Role::Agent).await;

    let err = client
        .update_page(UpdatePageRequest {
            page_id: "markdown/instances/customer/x.md".to_owned(),
            content: "---\ntype: instance\nskill: customer\nid: x\nname: X\n---\n# X\n".to_owned(),
            ..Default::default()
        })
        .await
        .expect_err("a reader replica refuses every mutating tool");
    match err {
        Error::JsonRpc {
            code,
            data_code,
            retryable,
            ..
        } => {
            assert_eq!(
                data_code.as_deref(),
                Some("read_only_replica"),
                "the STABLE code from error.data, not the numeric {code}"
            );
            assert_eq!(retryable, Some(true), "replicas retry against the writer");
        }
        other => panic!("expected Error::JsonRpc with data, got {other:?}"),
    }
    p.shutdown().await;
}

/// An error envelope WITHOUT `data` (plain protocol errors) keeps both
/// additions `None` — back-compat for every existing branch.
#[tokio::test]
async fn error_without_data_decodes_none() {
    let p = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: None,
        config_overrides: ConfigOverrides::default(),
    })
    .await;
    let client = client_as(&p, Role::Agent).await;
    let err = client
        .call_raw("no_such_tool", json!({}))
        .await
        .expect_err("unknown tool is a JSON-RPC error");
    match err {
        Error::JsonRpc {
            data_code,
            retryable,
            ..
        } => {
            assert!(data_code.is_none(), "no data → no data_code");
            assert!(retryable.is_none(), "no data → no retryable");
        }
        other => panic!("expected Error::JsonRpc, got {other:?}"),
    }
    p.shutdown().await;
}
