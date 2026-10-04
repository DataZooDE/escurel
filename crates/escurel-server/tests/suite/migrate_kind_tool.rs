//! The hard cut through the real gateway: the removed `type:` page-kind key is refused with a
//! named error, `kind:` works end to end, and the `migrate_kind` admin tool (dry run by default,
//! admin-only, tenant-checked) rewrites a genuinely LEGACY store.
//!
//! The legacy store is built the way an old one really is: pages in the lane that no index ever
//! parsed. Real gateway, real DuckDB + file store, real OIDC (TestIssuer), real reqwest. No mocks.

use std::sync::Arc;

use bytes::Bytes;
use duckdb::Connection;
use escurel_embed::{Embedder, ZeroEmbedder};
use escurel_index::{Indexer, Migrator};
use escurel_storage::{FsStore, Key, LaneStore};
use escurel_test_support::{AuthMode, ConfigOverrides, EscurelProcess, Opts, Role};
use serde_json::{Value, json};
use tempfile::TempDir;

const TENANT: &str = "acme";
const SKILL_PATH: &str = "markdown/skills/customer.md";
const INSTANCE_PATH: &str = "markdown/instances/customer/acme-corp.md";
const LEGACY_SKILL: &str = "---\ntype: skill\nid: customer\ndescription: x\n---\n# customer\n";
const LEGACY_INSTANCE: &str = "---\ntype: instance\nskill: customer\nid: acme-corp\n---\n# Acme\n";

struct Harness {
    process: EscurelProcess,
    _store: TempDir,
    _db: TempDir,
}

/// A gateway over a store whose lane holds LEGACY pages (written straight to the lane).
async fn start_with_legacy_lane() -> Harness {
    start_with_legacy_lane_quarantined(false).await
}

/// `quarantine`: boot the tenant the way the server does when its lane holds legacy pages.
async fn start_with_legacy_lane_quarantined(quarantine: bool) -> Harness {
    let store_dir = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    let store: Arc<dyn LaneStore> = Arc::new(FsStore::new(store_dir.path().to_path_buf()));
    for (path, md) in [(SKILL_PATH, LEGACY_SKILL), (INSTANCE_PATH, LEGACY_INSTANCE)] {
        store
            .write(&Key::new(TENANT, path.to_owned()).unwrap(), Bytes::from(md))
            .await
            .unwrap();
    }
    let conn = Connection::open(db_dir.path().join("escurel.duckdb")).unwrap();
    Migrator::up(&conn).unwrap();
    let embedder: Arc<dyn Embedder> = Arc::new(ZeroEmbedder::default());
    let indexer = Arc::new(Indexer::new(store, embedder, conn, TENANT).unwrap());
    if quarantine {
        assert!(indexer.quarantine_legacy_kind_pages().await.unwrap());
    }
    let process = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: None,
        config_overrides: ConfigOverrides {
            indexer: Some(indexer),
            ..Default::default()
        },
    })
    .await;
    Harness {
        process,
        _store: store_dir,
        _db: db_dir,
    }
}

async fn call(p: &EscurelProcess, role: Role, name: &str, args: Value) -> Value {
    let token = p.mint_token(TENANT, role);
    reqwest::Client::new()
        .post(p.mcp_url())
        .header("authorization", format!("Bearer {token}"))
        .json(&json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": { "name": name, "arguments": args },
        }))
        .send()
        .await
        .expect("post")
        .json()
        .await
        .expect("json")
}

fn structured(body: &Value) -> &Value {
    assert!(body.get("error").is_none(), "tool error: {body}");
    &body["result"]["structuredContent"]
}

fn pages(v: &Value) -> Vec<String> {
    v.as_array()
        .expect("array")
        .iter()
        .map(|p| p.as_str().expect("str").to_owned())
        .collect()
}

#[tokio::test]
async fn migrate_kind_is_a_dry_run_unless_apply_is_set_and_apply_is_idempotent() {
    let h = start_with_legacy_lane().await;
    let p = &h.process;

    // No `apply`: a dry run. It reports the legacy pages and writes nothing.
    let dry = call(
        p,
        Role::Admin,
        "migrate_kind",
        json!({ "tenant_id": TENANT }),
    )
    .await;
    let r = structured(&dry);
    assert_eq!(r["applied"], false);
    let mut would = pages(&r["pages_to_migrate"]);
    would.sort();
    assert_eq!(would, vec![INSTANCE_PATH.to_owned(), SKILL_PATH.to_owned()]);
    assert!(
        r["audit_event_id"].is_null(),
        "a dry run records no audit event"
    );

    // A second dry run reports exactly the same: nothing was written.
    let dry2 = call(
        p,
        Role::Admin,
        "migrate_kind",
        json!({ "tenant_id": TENANT }),
    )
    .await;
    assert_eq!(pages(&structured(&dry2)["pages_to_migrate"]), would);

    // apply: rewritten and audited.
    let done = call(
        p,
        Role::Admin,
        "migrate_kind",
        json!({ "tenant_id": TENANT, "apply": true }),
    )
    .await;
    let r = structured(&done);
    assert_eq!(r["applied"], true);
    let mut migrated = pages(&r["pages_to_migrate"]);
    migrated.sort();
    assert_eq!(migrated, would);
    assert!(
        r["audit_event_id"].is_string(),
        "an applied migration is audited"
    );

    // The migrated pages are served: reads go through the rewritten lane and index.
    let page = call(
        p,
        Role::Agent,
        "expand",
        json!({ "page_id": INSTANCE_PATH }),
    )
    .await;
    assert!(
        page.get("error").is_none(),
        "expand after migration: {page}"
    );

    // Idempotent: nothing left to migrate.
    let again = call(
        p,
        Role::Admin,
        "migrate_kind",
        json!({ "tenant_id": TENANT, "apply": true }),
    )
    .await;
    assert!(pages(&structured(&again)["pages_to_migrate"]).is_empty());
}

#[tokio::test]
async fn migrate_kind_needs_the_admin_role_and_the_right_tenant() {
    let h = start_with_legacy_lane().await;
    let p = &h.process;
    let agent = call(
        p,
        Role::Agent,
        "migrate_kind",
        json!({ "tenant_id": TENANT }),
    )
    .await;
    assert_eq!(
        agent["error"]["code"], -32001,
        "a non-admin is refused: {agent}"
    );
    let foreign = call(
        p,
        Role::Admin,
        "migrate_kind",
        json!({ "tenant_id": "globex" }),
    )
    .await;
    assert_eq!(
        foreign["error"]["code"], -32002,
        "a foreign tenant is refused: {foreign}"
    );
}

#[tokio::test]
async fn the_removed_type_key_is_refused_with_a_named_error_and_kind_works_end_to_end() {
    let h = start_with_legacy_lane().await;
    let p = &h.process;

    // validate: a structured issue, not a generic parse failure.
    let v = call(
        p,
        Role::Agent,
        "validate",
        json!({ "content": "---\ntype: instance\nskill: customer\nid: c9\n---\n# c9\n" }),
    )
    .await;
    let issues = structured(&v)["issues"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let issue = issues
        .iter()
        .find(|i| i["code"] == "frontmatter_type_removed")
        .unwrap_or_else(|| panic!("expected frontmatter_type_removed in {issues:?}"));
    assert_eq!(issue["location"], "frontmatter.type");
    assert!(
        issue["suggestion"]
            .as_str()
            .unwrap_or("")
            .contains("escurel admin migrate-kind")
    );

    // update_page with the removed key: refused as an actionable `{ok:false, issues}`, nothing lands.
    let w = call(
        p,
        Role::Agent,
        "update_page",
        json!({
            "page_id": "markdown/instances/customer/c9.md",
            "content": "---\ntype: instance\nskill: customer\nid: c9\n---\n# c9\n",
        }),
    )
    .await;
    let text = w.to_string();
    assert!(text.contains("frontmatter_type_removed"), "{w}");

    // Migrate, then the SAME page with `kind:` is written and read back.
    let applied = call(
        p,
        Role::Admin,
        "migrate_kind",
        json!({ "tenant_id": TENANT, "apply": true }),
    )
    .await;
    structured(&applied);
    let ok = call(
        p,
        Role::Agent,
        "update_page",
        json!({
            "page_id": "markdown/instances/customer/c9.md",
            "content": "---\nkind: instance\nskill: customer\nid: c9\n---\n# c9\n",
        }),
    )
    .await;
    assert!(ok.get("error").is_none(), "kind: is written: {ok}");
    let back = call(
        p,
        Role::Agent,
        "expand",
        json!({ "page_id": "markdown/instances/customer/c9.md" }),
    )
    .await;
    assert!(back.get("error").is_none(), "and read back: {back}");
}

#[tokio::test]
async fn a_quarantined_tenant_serves_nothing_but_the_migration_until_it_is_migrated() {
    let h = start_with_legacy_lane_quarantined(true).await;
    let p = &h.process;

    // Everything else is refused with a named error that carries the command.
    let search = call(p, Role::Agent, "search", json!({ "q": "acme" })).await;
    let err = &search["error"];
    assert!(
        !err.is_null(),
        "a quarantined tenant must not serve: {search}"
    );
    assert_eq!(err["data"]["code"], "tenant_quarantined", "{search}");
    let msg = err["message"].as_str().unwrap_or_default();
    assert!(msg.contains("escurel admin migrate-kind"), "{msg}");
    let expand = call(
        p,
        Role::Agent,
        "expand",
        json!({ "page_id": INSTANCE_PATH }),
    )
    .await;
    assert_eq!(
        expand["error"]["data"]["code"], "tenant_quarantined",
        "{expand}"
    );

    // The migration itself still runs, and reports the quarantine.
    let dry = call(
        p,
        Role::Admin,
        "migrate_kind",
        json!({ "tenant_id": TENANT }),
    )
    .await;
    assert_eq!(structured(&dry)["tenant_quarantined"], true);

    let done = call(
        p,
        Role::Admin,
        "migrate_kind",
        json!({ "tenant_id": TENANT, "apply": true }),
    )
    .await;
    assert_eq!(structured(&done)["tenant_quarantined"], false, "{done}");

    // Lifted: the tenant serves, from a freshly rebuilt index.
    let page = call(
        p,
        Role::Agent,
        "expand",
        json!({ "page_id": INSTANCE_PATH }),
    )
    .await;
    assert!(
        page.get("error").is_none(),
        "served after migration: {page}"
    );
    let found = call(p, Role::Agent, "search", json!({ "q": "Acme" })).await;
    assert!(found.get("error").is_none(), "{found}");
}

// ---- the quarantine covers EVERY door, not only POST /mcp tools/call -------------------------------
//
// A quarantined tenant has a half-built index. It used to refuse MCP tools but still took writes on
// /ingest, /ingest/upload and a live /ws, and served /blob.

async fn quarantined_status(
    p: &EscurelProcess,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> (u16, Value) {
    let token = p.mint_token(TENANT, Role::Agent);
    let url = format!("{}{path}", p.base_url());
    let client = reqwest::Client::new();
    let req = if method == "GET" {
        client.get(url)
    } else {
        client.post(url).json(&body.unwrap_or(Value::Null))
    };
    let resp = req
        .header("authorization", format!("Bearer {token}"))
        .send()
        .await
        .expect("request");
    let status = resp.status().as_u16();
    let json = resp.json::<Value>().await.unwrap_or(Value::Null);
    (status, json)
}

#[tokio::test]
async fn a_quarantined_tenant_refuses_a_malformed_ingest_with_the_quarantine_not_a_422() {
    // The quarantine is checked BEFORE the body is validated: a client that has not read the notice
    // and sends a malformed body must still be told the tenant is waiting for its migration.
    let h = start_with_legacy_lane_quarantined(true).await;
    for body in [json!({}), json!({ "nonsense": 1 })] {
        let (status, json) = quarantined_status(&h.process, "POST", "/ingest", Some(body)).await;
        assert_eq!(status, 503, "{json}");
        assert_eq!(json["error"], "tenant_quarantined", "{json}");
    }
}

#[tokio::test]
async fn a_quarantined_tenant_refuses_ingest_upload_and_blob_reads() {
    let h = start_with_legacy_lane_quarantined(true).await;
    let p = &h.process;
    let cases = [
        (
            "POST",
            "/ingest",
            Some(json!({ "blob_id": "b1", "content_type": "text/plain" })),
        ),
        (
            "POST",
            "/ingest/upload",
            Some(json!({ "content_type": "text/plain", "bytes_b64": "aGVsbG8=" })),
        ),
        (
            "GET",
            "/blob/markdown/instances/customer/acme-corp.md",
            None,
        ),
    ];
    for (method, path, body) in cases {
        let (status, json) = quarantined_status(p, method, path, body).await;
        assert_eq!(
            status, 503,
            "{method} {path} must be refused while quarantined, got {status} {json}"
        );
        assert_eq!(
            json["error"], "tenant_quarantined",
            "{method} {path}: {json}"
        );
        assert!(
            json["message"]
                .as_str()
                .unwrap_or_default()
                .contains("escurel admin migrate-kind"),
            "the refusal names the remedy: {json}"
        );
    }
}

#[tokio::test]
async fn a_quarantined_tenant_refuses_a_live_websocket() {
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    let h = start_with_legacy_lane_quarantined(true).await;
    let p = &h.process;
    let token = p.mint_token(TENANT, Role::Agent);
    let mut req = p.ws_url().into_client_request().unwrap();
    req.headers_mut()
        .insert("authorization", format!("Bearer {token}").parse().unwrap());
    match tokio_tungstenite::connect_async(req).await {
        Err(tokio_tungstenite::tungstenite::Error::Http(resp)) => {
            assert_eq!(resp.status().as_u16(), 503, "the upgrade is refused");
        }
        other => panic!("a quarantined tenant must refuse the socket, got {other:?}"),
    }
}
