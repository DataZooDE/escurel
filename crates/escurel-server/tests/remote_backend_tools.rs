//! End-to-end wire tests for the remote (proxy) instance backends,
//! `openapi` and `mcp`.
//!
//! Real gateway (`POST /mcp`), real OIDC (`TestIssuer` JWKS), real DuckDB +
//! `FsStore`, real reqwest — and a **real, stateful upstream service** per
//! kind, bound to a loopback port:
//!   - `openapi`: a tiny `axum` CRM whose `GET` returns the customer's current
//!     row and whose `PATCH` genuinely mutates it.
//!   - `mcp`: a JSON-RPC 2.0 server answering `tools/list` / `tools/call`,
//!     backed by a real article store whose `putArticle` mutates state.
//!
//! Nothing is canned, so a write that fails to propagate (or one the ACL
//! should have blocked) is caught by the next read — the whole point of
//! exercising the real boundary. The upstreams are genuine networked
//! dependencies, not doubles of escurel's own code; the boundary each test
//! covers is escurel's proxy machinery (skill binding → endpoint registry →
//! outbound `reqwest` → JSONPath projection → ACL-gated write-back).

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use duckdb::Connection;
use escurel_embed::{Embedder, ZeroEmbedder};
use escurel_index::{Indexer, Migrator};
use escurel_storage::{FsStore, LaneStore};
use escurel_test_support::{AuthMode, ConfigOverrides, EscurelProcess, Opts, Role};
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::net::TcpListener;

const TENANT: &str = "acme";

// --- shared gateway + tool-call helpers --------------------------------

/// Spawn a gateway over a real DuckDB + `FsStore`, seeded with one skill.
async fn spawn_gateway(skill_id: &str, skill_md: &str) -> (EscurelProcess, Vec<TempDir>) {
    let store_dir = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    let store: Arc<dyn LaneStore> = Arc::new(FsStore::new(store_dir.path().to_path_buf()));
    let embedder: Arc<dyn Embedder> = Arc::new(ZeroEmbedder::default());
    let conn = Connection::open(db_dir.path().join("escurel.duckdb")).unwrap();
    Migrator::up(&conn).unwrap();
    let indexer = Arc::new(Indexer::new(store, embedder, conn, TENANT).unwrap());
    indexer
        .update_page(&format!("markdown/skills/{skill_id}.md"), skill_md)
        .await
        .unwrap();

    let process = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        config_overrides: ConfigOverrides {
            indexer: Some(indexer),
            ..Default::default()
        },
        ..Default::default()
    })
    .await;
    (process, vec![store_dir, db_dir])
}

/// POST a `tools/call` as `role` and return the full JSON-RPC envelope.
async fn call(p: &EscurelProcess, role: Role, name: &str, args: Value) -> Value {
    let token = p.mint_token(TENANT, role);
    reqwest::Client::new()
        .post(p.mcp_url())
        .header("authorization", format!("Bearer {token}"))
        .json(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": { "name": name, "arguments": args },
        }))
        .send()
        .await
        .expect("post")
        .json()
        .await
        .expect("json")
}

/// `expand` the instance and return one live-projected `backend_projection`
/// field as a string.
async fn live_field(p: &EscurelProcess, page_id: &str, field: &str) -> String {
    let body = call(p, Role::Admin, "expand", json!({ "page_id": page_id })).await;
    body["result"]["structuredContent"]["backend_projection"]["fields"][field]
        .as_str()
        .unwrap_or_else(|| panic!("no live `{field}` in expand: {body}"))
        .to_owned()
}

// --- openapi: a real, stateful CRM over REST ---------------------------

const CUSTOMER_SKILL: &str = "---\n\
     type: skill\n\
     id: customer\n\
     description: CRM customers, proxied live over REST.\n\
     backend:\n\
    \x20 kind: openapi\n\
    \x20 endpoint: crm_rest\n\
    \x20 read: { path: \"/customers/{id}\" }\n\
    \x20 write: { method: PATCH, path: \"/customers/{id}\" }\n\
    \x20 project: { display_name: $.name, tier: $.account_tier }\n\
     ---\n\
     # customer\n";

/// id → customer object; a PATCH mutates the row so the next GET sees it.
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

/// Start the CRM on an ephemeral loopback port, seeded with `acme` at `gold`.
async fn start_crm() -> (String, tokio::task::JoinHandle<()>) {
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
    let (base, handle) = serve(app).await;
    (base, handle)
}

#[tokio::test]
async fn openapi_remote_backend_read_write_over_the_wire() {
    let (base_url, _crm) = start_crm().await;
    let (process, _dirs) = spawn_gateway("customer", CUSTOMER_SKILL).await;
    let p = &process;

    // 1. Register the endpoint pointing at the live CRM.
    let reg = call(
        p,
        Role::Admin,
        "register_endpoint",
        json!({ "name": "crm_rest", "kind": "openapi", "base_url": base_url }),
    )
    .await;
    assert!(reg.get("error").is_none(), "register error: {reg}");
    assert_eq!(reg["result"]["structuredContent"]["ok"], true);

    // 2. Probe reachability.
    let val = call(p, Role::Admin, "validate_endpoints", json!({})).await;
    assert_eq!(
        val["result"]["structuredContent"]["ok"], true,
        "endpoint should be reachable: {val}"
    );

    // 3. Materialise the overlay instance.
    let created = call(
        p,
        Role::Admin,
        "create_remote_instance",
        json!({ "skill": "customer", "id": "acme" }),
    )
    .await;
    assert!(created.get("error").is_none(), "create error: {created}");
    let page_id = created["result"]["structuredContent"]["page_id"]
        .as_str()
        .expect("page_id")
        .to_owned();
    assert_eq!(created["result"]["structuredContent"]["kind"], "openapi");

    // 4. expand → live GET.
    let body = call(p, Role::Admin, "expand", json!({ "page_id": page_id })).await;
    let proj = &body["result"]["structuredContent"]["backend_projection"];
    assert_eq!(proj["source"], "crm_rest", "projection source: {body}");
    assert_eq!(proj["fields"]["display_name"], "Acme Corp", "GET: {body}");
    assert_eq!(proj["fields"]["tier"], "gold", "GET: {body}");

    // 5. write_instance (admin) → PATCH mutates the CRM; read-after-write
    //    confirms the upstream state actually changed.
    let written = call(
        p,
        Role::Admin,
        "write_instance",
        json!({ "ref": "customer::acme", "payload": { "account_tier": "platinum" } }),
    )
    .await;
    assert!(written.get("error").is_none(), "write error: {written}");
    let w = &written["result"]["structuredContent"];
    assert_eq!(w["ok"], true, "write result: {written}");
    assert_eq!(w["fields"]["tier"], "platinum", "write echo: {written}");
    assert_eq!(
        live_field(p, &page_id, "tier").await,
        "platinum",
        "read-after-write must observe the real upstream mutation"
    );

    // 6. write_instance (agent) → refused; the CRM must be untouched.
    let denied = call(
        p,
        Role::Agent,
        "write_instance",
        json!({ "ref": "customer::acme", "payload": { "account_tier": "bronze" } }),
    )
    .await;
    assert!(
        denied.get("error").is_some(),
        "agent write must be refused: {denied}"
    );
    assert_eq!(
        live_field(p, &page_id, "tier").await,
        "platinum",
        "the refused write must never have reached the CRM"
    );

    process.shutdown().await;
}

// --- mcp: a real, stateful JSON-RPC KB ---------------------------------

const ARTICLE_SKILL: &str = "---\n\
     type: skill\n\
     id: article\n\
     description: KB articles, proxied live over MCP.\n\
     backend:\n\
    \x20 kind: mcp\n\
    \x20 endpoint: upstream_kb\n\
    \x20 read:  { tool: getArticle }\n\
    \x20 write: { tool: putArticle }\n\
    \x20 project: { title: $.title, status: $.status }\n\
     ---\n\
     # article\n";

/// id → article object; `putArticle` mutates it, `getArticle` reads it.
type Kb = Arc<Mutex<std::collections::BTreeMap<String, Value>>>;

/// A JSON-RPC 2.0 MCP endpoint. `escurel` POSTs bare method calls (no
/// `initialize` handshake) — `tools/list` for the probe, `tools/call` for
/// read/write. Returns `structuredContent` the projection reads directly.
async fn mcp_rpc(State(kb): State<Kb>, Json(req): Json<Value>) -> Json<Value> {
    let id = req.get("id").cloned().unwrap_or(json!(1));
    let method = req["method"].as_str().unwrap_or_default();
    let params = &req["params"];
    let result = match method {
        "tools/list" => json!({
            "tools": [ { "name": "getArticle" }, { "name": "putArticle" } ]
        }),
        "tools/call" => {
            let name = params["name"].as_str().unwrap_or_default();
            let args = &params["arguments"];
            let aid = args["id"].as_str().unwrap_or_default().to_owned();
            let mut guard = kb.lock().unwrap();
            let article = guard.entry(aid).or_insert_with(|| json!({}));
            if name == "putArticle"
                && let (Some(obj), Some(patch)) = (article.as_object_mut(), args.as_object())
            {
                for (k, v) in patch {
                    if k != "id" {
                        obj.insert(k.clone(), v.clone());
                    }
                }
            }
            json!({ "structuredContent": article.clone(), "content": [], "isError": false })
        }
        other => {
            return Json(json!({
                "jsonrpc": "2.0", "id": id,
                "error": { "code": -32601, "message": format!("unknown method `{other}`") }
            }));
        }
    };
    Json(json!({ "jsonrpc": "2.0", "id": id, "result": result }))
}

/// Start the KB on an ephemeral loopback port, seeded with `welcome`
/// (`draft`). The endpoint base URL points at its `/mcp` path.
async fn start_kb() -> (String, tokio::task::JoinHandle<()>) {
    let kb: Kb = Arc::new(Mutex::new(
        [(
            "welcome".to_owned(),
            json!({ "title": "Welcome", "status": "draft" }),
        )]
        .into_iter()
        .collect(),
    ));
    let app = Router::new().route("/mcp", post(mcp_rpc)).with_state(kb);
    let (base, handle) = serve(app).await;
    (format!("{base}/mcp"), handle)
}

#[tokio::test]
async fn mcp_remote_backend_read_write_over_the_wire() {
    let (base_url, _kb) = start_kb().await;
    let (process, _dirs) = spawn_gateway("article", ARTICLE_SKILL).await;
    let p = &process;

    // 1. Register the MCP endpoint pointing at the live KB `/mcp`.
    let reg = call(
        p,
        Role::Admin,
        "register_endpoint",
        json!({ "name": "upstream_kb", "kind": "mcp", "base_url": base_url }),
    )
    .await;
    assert!(reg.get("error").is_none(), "register error: {reg}");
    assert_eq!(reg["result"]["structuredContent"]["ok"], true);

    // 2. Probe → the KB answers `tools/list`.
    let val = call(p, Role::Admin, "validate_endpoints", json!({})).await;
    assert_eq!(
        val["result"]["structuredContent"]["ok"], true,
        "mcp endpoint should be reachable via tools/list: {val}"
    );

    // 3. Materialise the overlay instance.
    let created = call(
        p,
        Role::Admin,
        "create_remote_instance",
        json!({ "skill": "article", "id": "welcome" }),
    )
    .await;
    assert!(created.get("error").is_none(), "create error: {created}");
    let page_id = created["result"]["structuredContent"]["page_id"]
        .as_str()
        .expect("page_id")
        .to_owned();
    assert_eq!(created["result"]["structuredContent"]["kind"], "mcp");

    // 4. expand → live `tools/call` getArticle, projected.
    let body = call(p, Role::Admin, "expand", json!({ "page_id": page_id })).await;
    let proj = &body["result"]["structuredContent"]["backend_projection"];
    assert_eq!(proj["source"], "upstream_kb", "projection source: {body}");
    assert_eq!(proj["fields"]["title"], "Welcome", "getArticle: {body}");
    assert_eq!(proj["fields"]["status"], "draft", "getArticle: {body}");

    // 5. write_instance (admin) → putArticle mutates the KB; read-after-write
    //    confirms the upstream state actually changed.
    let written = call(
        p,
        Role::Admin,
        "write_instance",
        json!({ "ref": "article::welcome", "payload": { "status": "published" } }),
    )
    .await;
    assert!(written.get("error").is_none(), "write error: {written}");
    let w = &written["result"]["structuredContent"];
    assert_eq!(w["ok"], true, "write result: {written}");
    assert_eq!(w["fields"]["status"], "published", "write echo: {written}");
    assert_eq!(
        live_field(p, &page_id, "status").await,
        "published",
        "read-after-write must observe the real upstream mutation"
    );

    // 6. write_instance (agent) → refused; the KB must be untouched.
    let denied = call(
        p,
        Role::Agent,
        "write_instance",
        json!({ "ref": "article::welcome", "payload": { "status": "trashed" } }),
    )
    .await;
    assert!(
        denied.get("error").is_some(),
        "agent write must be refused: {denied}"
    );
    assert_eq!(
        live_field(p, &page_id, "status").await,
        "published",
        "the refused write must never have reached the KB"
    );

    process.shutdown().await;
}

// --- openapi: auth forwarding + fail-closed degraded reads -------------

/// The bearer token the guarded CRM demands. Registered server-side under the
/// endpoint; it must be forwarded on the outbound read and never echoed back.
const SECRET: &str = "s3cr3t-token-42";

/// A CRM that 401s every read lacking `Authorization: Bearer <SECRET>`.
async fn get_customer_guarded(
    headers: HeaderMap,
    Path(id): Path<String>,
    State(db): State<Crm>,
) -> Response {
    let expected = format!("Bearer {SECRET}");
    let authed = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v == expected);
    if !authed {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({ "error": "unauthorised" })),
        )
            .into_response();
    }
    let row = db.lock().unwrap().get(&id).cloned().unwrap_or(Value::Null);
    Json(row).into_response()
}

async fn start_crm_guarded() -> (String, tokio::task::JoinHandle<()>) {
    let db: Crm = Arc::new(Mutex::new(
        [(
            "acme".to_owned(),
            json!({ "name": "Acme Corp", "account_tier": "gold" }),
        )]
        .into_iter()
        .collect(),
    ));
    let app = Router::new()
        .route("/customers/{id}", get(get_customer_guarded))
        .with_state(db);
    serve(app).await
}

/// A registered bearer secret is forwarded on the outbound read (the guarded
/// upstream 200s) and never surfaced by `list_endpoints`. Flipping the same
/// endpoint to `auth=none` makes the upstream 401 — and the read fails closed
/// to a `{ issue }`, never a fabricated body.
#[tokio::test]
async fn openapi_bearer_auth_forwarded_and_read_degrades_when_unauthorised() {
    let (base_url, _crm) = start_crm_guarded().await;
    let (process, _dirs) = spawn_gateway("customer", CUSTOMER_SKILL).await;
    let p = &process;

    // Register with the correct bearer secret.
    let reg = call(
        p,
        Role::Admin,
        "register_endpoint",
        json!({
            "name": "crm_rest", "kind": "openapi",
            "base_url": base_url, "auth": "bearer", "secret": SECRET,
        }),
    )
    .await;
    assert!(reg.get("error").is_none(), "register error: {reg}");

    // The secret is never echoed back (REQ-REMOTE-05).
    let list = call(p, Role::Admin, "list_endpoints", json!({})).await;
    assert!(
        !list.to_string().contains(SECRET),
        "secret leaked through list_endpoints: {list}"
    );

    let created = call(
        p,
        Role::Admin,
        "create_remote_instance",
        json!({ "skill": "customer", "id": "acme" }),
    )
    .await;
    let page_id = created["result"]["structuredContent"]["page_id"]
        .as_str()
        .expect("page_id")
        .to_owned();

    // Auth forwarded → upstream 200 → fields projected (no issue).
    let body = call(
        p,
        Role::Admin,
        "expand",
        json!({ "page_id": page_id.clone() }),
    )
    .await;
    let proj = &body["result"]["structuredContent"]["backend_projection"];
    assert!(
        proj.get("issue").is_none(),
        "authorised read must not degrade: {body}"
    );
    assert_eq!(
        proj["fields"]["display_name"], "Acme Corp",
        "bearer must have been forwarded: {body}"
    );

    // Flip the endpoint to auth=none (idempotent upsert) → upstream 401.
    let reg2 = call(
        p,
        Role::Admin,
        "register_endpoint",
        json!({ "name": "crm_rest", "kind": "openapi", "base_url": base_url, "auth": "none" }),
    )
    .await;
    assert!(reg2.get("error").is_none(), "re-register error: {reg2}");

    // The now-unauthorised read fails closed to an issue, no fabricated fields.
    let body = call(p, Role::Admin, "expand", json!({ "page_id": page_id })).await;
    let proj = &body["result"]["structuredContent"]["backend_projection"];
    assert!(
        proj["issue"].is_string(),
        "unauthorised read must degrade to an issue: {body}"
    );
    assert!(
        proj.get("fields").is_none(),
        "a degraded read must not carry fields: {body}"
    );

    process.shutdown().await;
}

/// An endpoint whose upstream is unreachable (nothing listening) fails closed:
/// `validate_endpoints` reports it unreachable, and a live read degrades to a
/// `{ issue }` rather than fabricating a body.
#[tokio::test]
async fn openapi_unreachable_endpoint_degrades_read_and_probe() {
    // Bind then drop, so the port is closed (connection refused).
    let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .unwrap();
    let dead = format!("http://{}", listener.local_addr().unwrap());
    drop(listener);

    let (process, _dirs) = spawn_gateway("customer", CUSTOMER_SKILL).await;
    let p = &process;

    let reg = call(
        p,
        Role::Admin,
        "register_endpoint",
        json!({ "name": "crm_rest", "kind": "openapi", "base_url": dead }),
    )
    .await;
    assert!(reg.get("error").is_none(), "register error: {reg}");

    // Probe reports it unreachable.
    let val = call(p, Role::Admin, "validate_endpoints", json!({})).await;
    let v = &val["result"]["structuredContent"];
    assert_eq!(v["ok"], false, "dead endpoint must fail validation: {val}");
    assert_eq!(v["unreachable"], 1, "one unreachable endpoint: {val}");
    assert_eq!(v["endpoints"][0]["status"], "unreachable", "probe: {val}");

    // Live read degrades to an issue.
    let created = call(
        p,
        Role::Admin,
        "create_remote_instance",
        json!({ "skill": "customer", "id": "acme" }),
    )
    .await;
    let page_id = created["result"]["structuredContent"]["page_id"]
        .as_str()
        .expect("page_id")
        .to_owned();
    let body = call(p, Role::Admin, "expand", json!({ "page_id": page_id })).await;
    let proj = &body["result"]["structuredContent"]["backend_projection"];
    assert!(
        proj["issue"].is_string(),
        "unreachable upstream must degrade to an issue: {body}"
    );

    process.shutdown().await;
}

/// An endpoint registered under a different `kind` than the skill's backend
/// must fail the read closed — not silently dispatch the wrong transport.
/// `create_remote_instance` only checks the endpoint *name* exists, so this
/// guard is the last line of defence. Here an `openapi` skill points at an
/// endpoint mis-registered as `mcp` (but backed by the real OpenAPI CRM):
/// without the guard the HTTP read would just succeed; with it, it degrades.
#[tokio::test]
async fn remote_read_fails_closed_on_endpoint_protocol_mismatch() {
    let (base_url, _crm) = start_crm().await;
    let (process, _dirs) = spawn_gateway("customer", CUSTOMER_SKILL).await;
    let p = &process;

    // Same name the skill references, but the WRONG kind.
    let reg = call(
        p,
        Role::Admin,
        "register_endpoint",
        json!({ "name": "crm_rest", "kind": "mcp", "base_url": base_url }),
    )
    .await;
    assert!(reg.get("error").is_none(), "register error: {reg}");

    let created = call(
        p,
        Role::Admin,
        "create_remote_instance",
        json!({ "skill": "customer", "id": "acme" }),
    )
    .await;
    assert!(created.get("error").is_none(), "create error: {created}");
    let page_id = created["result"]["structuredContent"]["page_id"]
        .as_str()
        .expect("page_id")
        .to_owned();

    let body = call(p, Role::Admin, "expand", json!({ "page_id": page_id })).await;
    let proj = &body["result"]["structuredContent"]["backend_projection"];
    let issue = proj["issue"]
        .as_str()
        .unwrap_or_else(|| panic!("protocol mismatch must degrade to an issue: {body}"));
    assert!(
        issue.contains("openapi") && issue.contains("mcp"),
        "the issue should name both the skill and endpoint kinds: {issue}"
    );

    process.shutdown().await;
}

// --- upstream bootstrap ------------------------------------------------

/// Bind an ephemeral loopback port, serve `app`, and return `(base_url, task)`.
async fn serve(app: Router) -> (String, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://{addr}"), handle)
}
