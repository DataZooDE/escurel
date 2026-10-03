//! `instances: rows` over a REAL MCP server (stage 4b): the spec's streamable-HTTP transport, not a
//! bare JSON-RPC POST.
//!
//! The upstream (`mcp_upstream.rs`) enforces the handshake, the session and protocol-version
//! headers, answers listings as SSE, and can expire its sessions; it records every request. So the
//! tests prove what crossed the wire: ONE handshake for many calls, the headers on every call,
//! transparent re-initialisation, and that nothing the server says about itself reaches an agent.

use std::sync::Arc;
use std::sync::atomic::Ordering;

use escurel_test_support::Role;
use serde_json::{Value, json};

use super::mcp_upstream::{INJECTION, PROTOCOL, Upstream, start};
use super::remote_support::{admin, call_as, loopback_ok, spawn_gateway};

const ARTICLE_SKILL: &str = "---\n\
     kind: skill\n\
     id: article\n\
     description: Knowledge-base articles, one instance per article of an MCP server.\n\
     backend:\n\
    \x20 kind: mcp\n\
    \x20 endpoint: upstream_kb\n\
    \x20 instances: rows\n\
    \x20 key: $.slug\n\
    \x20 linked: true\n\
    \x20 list:\n\
    \x20   tool: listArticles\n\
    \x20   items: $.articles\n\
    \x20   limit_param: limit\n\
    \x20   cursor: { arg: after, from: $.next }\n\
    \x20 read: { tool: getArticle }\n\
    \x20 project: { title: $.title }\n\
     ---\n\
     # article\n";

async fn gateway_over(url: &str) -> (escurel_test_support::EscurelProcess, Vec<tempfile::TempDir>) {
    let (p, dirs) = spawn_gateway(&[("article", ARTICLE_SKILL)], loopback_ok()).await;
    admin(
        &p,
        "register_endpoint",
        json!({ "name": "upstream_kb", "kind": "mcp", "base_url": url }),
    )
    .await;
    (p, dirs)
}

async fn list_all(p: &escurel_test_support::EscurelProcess, limit: u64) -> Vec<String> {
    let mut seen = Vec::new();
    let mut cursor: Option<String> = None;
    for _ in 0..50 {
        let mut args = json!({ "skill_id": "article", "limit": limit });
        if let Some(c) = &cursor {
            args["cursor"] = json!(c);
        }
        let page = admin(p, "list_instances", args).await;
        for i in page["instances"].as_array().unwrap() {
            seen.push(i["page_id"].as_str().unwrap().to_owned());
        }
        match page["next_cursor"].as_str() {
            Some(c) => cursor = Some(c.to_owned()),
            None => return seen,
        }
    }
    panic!("paging did not terminate");
}

#[tokio::test]
async fn one_handshake_serves_many_calls_and_every_call_carries_the_session_headers() {
    let up = Upstream::new(120);
    let url = start(&up).await;
    let (p, _dirs) = gateway_over(&url).await;

    let seen = list_all(&p, 50).await;

    assert_eq!(seen.len(), 120, "every article, once");
    let unique: std::collections::BTreeSet<_> = seen.iter().collect();
    assert_eq!(unique.len(), 120, "no duplicates across pages");
    assert_eq!(
        up.initializes.load(Ordering::SeqCst),
        1,
        "ONE initialize for all list calls"
    );
    assert_eq!(
        up.initialized_notes.load(Ordering::SeqCst),
        1,
        "notifications/initialized is sent once"
    );
    let calls = up.calls("tools/call");
    assert_eq!(calls.len(), 3, "120 articles at 50 per page = 3 list calls");
    for c in calls {
        assert!(
            c.session.as_deref().is_some_and(|s| s.starts_with("sess-")),
            "{c:?}"
        );
        assert_eq!(c.protocol.as_deref(), Some(PROTOCOL), "{c:?}");
    }
    p.shutdown().await;
}

#[tokio::test]
async fn expand_reads_one_article_through_the_get_tool_and_marks_it_external() {
    let up = Upstream::new(10);
    let url = start(&up).await;
    let (p, _dirs) = gateway_over(&url).await;

    let page = admin(
        &p,
        "expand",
        json!({ "page_id": "markdown/instances/article/a-0004.md" }),
    )
    .await;

    assert_eq!(page["backend_projection"]["trust"], "external", "{page}");
    assert_eq!(page["backend_projection"]["kind"], "mcp", "{page}");
    assert_eq!(page["frontmatter"]["title"], "Article 4", "{page}");
    let gone = admin(
        &p,
        "expand",
        json!({ "page_id": "markdown/instances/article/a-9999.md" }),
    )
    .await;
    assert!(
        gone["page"].is_null(),
        "an unknown article is absent: {gone}"
    );
    p.shutdown().await;
}

#[tokio::test]
async fn an_expired_session_is_reinitialised_transparently() {
    let up = Upstream::new(10);
    let url = start(&up).await;
    let (p, _dirs) = gateway_over(&url).await;
    let first = list_all(&p, 5).await;
    assert_eq!(first.len(), 10);
    assert_eq!(up.initializes.load(Ordering::SeqCst), 1);

    up.expire_sessions();
    let again = list_all(&p, 5).await;

    assert_eq!(
        again.len(),
        10,
        "the listing still works after the server forgot the session"
    );
    assert_eq!(
        up.initializes.load(Ordering::SeqCst),
        2,
        "exactly one re-initialisation"
    );
    p.shutdown().await;
}

#[tokio::test]
async fn nothing_the_server_says_about_itself_reaches_the_agent_wire() {
    let up = Upstream::new(10);
    let url = start(&up).await;
    let (p, _dirs) = gateway_over(&url).await;

    // Everything an agent or an admin can ask for that touches the upstream.
    let mut wire: Vec<Value> = Vec::new();
    wire.push(admin(&p, "list_instances", json!({ "skill_id": "article" })).await);
    wire.push(
        admin(
            &p,
            "expand",
            json!({ "page_id": "markdown/instances/article/a-0001.md" }),
        )
        .await,
    );
    wire.push(admin(&p, "validate_endpoints", json!({})).await);
    wire.push(admin(&p, "list_endpoints", json!({})).await);
    wire.push(admin(&p, "list_skills", json!({})).await);
    wire.push(admin(&p, "resolve", json!({ "wikilink": "[[article::a-0002]]" })).await);

    for w in &wire {
        assert!(
            !w.to_string().contains(INJECTION),
            "the upstream's own text reached the wire: {w}"
        );
    }
    // The probe DID talk to the server: it is not absence of traffic that proves this.
    assert!(
        !up.calls("tools/list").is_empty(),
        "validate_endpoints probes tools/list"
    );
    p.shutdown().await;
}

#[tokio::test]
async fn a_tool_error_degrades_to_a_bounded_message_without_the_servers_text_as_instructions() {
    let skill = ARTICLE_SKILL.replace("read: { tool: getArticle }", "read: { tool: failTool }");
    let up = Upstream::new(3);
    let url = start(&up).await;
    let (p, _dirs) = spawn_gateway(&[("article", skill.as_str())], loopback_ok()).await;
    admin(
        &p,
        "register_endpoint",
        json!({ "name": "upstream_kb", "kind": "mcp", "base_url": url }),
    )
    .await;

    let v = call_as(
        &p,
        Role::Admin,
        "expand",
        json!({ "page_id": "markdown/instances/article/a-0001.md" }),
    )
    .await;

    let text = v.to_string();
    assert!(
        v.get("error").is_some(),
        "an unreadable row with no notes is an error: {v}"
    );
    assert!(
        text.len() < 1_200,
        "the upstream's long error text must be bounded, got {} bytes",
        text.len()
    );
    p.shutdown().await;
}

#[tokio::test]
async fn the_bare_json_rpc_upstreams_of_the_old_tests_still_work_without_a_handshake() {
    // A server that answers `initialize` with "method not found" is a stateless/legacy upstream: the
    // client proceeds without a session instead of failing (the older remote_backend_tools tests
    // run against exactly such a server).
    use axum::Json;
    use axum::Router;
    use axum::routing::post;
    let app = Router::new().route(
        "/mcp",
        post(|Json(req): Json<Value>| async move {
            let id = req["id"].clone();
            if req["method"] == "initialize" {
                return Json(json!({ "jsonrpc": "2.0", "id": id,
                    "error": { "code": -32601, "message": "unknown method" } }));
            }
            Json(json!({ "jsonrpc": "2.0", "id": id,
                "result": { "structuredContent": { "slug": "a-1", "title": "Bare" }, "content": [] } }))
        }),
    );
    let (base, _h) = super::remote_support::serve(app).await;
    let (p, _dirs) = gateway_over(&format!("{base}/mcp")).await;

    let page = admin(
        &p,
        "expand",
        json!({ "page_id": "markdown/instances/article/a-1.md" }),
    )
    .await;

    assert_eq!(page["frontmatter"]["title"], "Bare", "{page}");
    p.shutdown().await;
}

#[allow(dead_code)]
fn _keep(_: Arc<Upstream>) {}

#[tokio::test]
async fn another_escurel_gateway_is_a_valid_mcp_upstream_an_independent_implementation() {
    // The upstream here is NOT our spec-faithful test server but a second, real escurel gateway: its
    // `/mcp` endpoint is separate server code, so a shared misunderstanding of the transport between
    // our client and our own test double would show up as a failure here.
    use escurel_test_support::{AuthMode, EscurelProcess, Opts};
    let upstream = EscurelProcess::spawn(Opts {
        auth: AuthMode::Disabled,
        ..Default::default()
    })
    .await;
    let skill = "---\n\
         kind: skill\n\
         id: remote_skill\n\
         description: the skills of ANOTHER escurel gateway, as rows.\n\
         backend:\n\
        \x20 kind: mcp\n\
        \x20 endpoint: other_gateway\n\
        \x20 instances: rows\n\
        \x20 key: $.id\n\
        \x20 list: { tool: list_skills, items: $.skills }\n\
        \x20 read: { tool: list_skills }\n\
        \x20 project: { description: $.description }\n\
         ---\n\
         # remote_skill\n";
    let (p, _dirs) = spawn_gateway(&[("remote_skill", skill)], loopback_ok()).await;
    admin(
        &p,
        "register_endpoint",
        json!({ "name": "other_gateway", "kind": "mcp", "base_url": upstream.mcp_url() }),
    )
    .await;

    let page = admin(&p, "list_instances", json!({ "skill_id": "remote_skill" })).await;

    let rows = page["instances"].as_array().expect("instances");
    assert!(
        !rows.is_empty(),
        "the other gateway lists at least its built-in skills: {page}"
    );
    assert!(
        rows.iter()
            .all(|r| r["trust"] == "external" && r["row"] == true),
        "{page}"
    );
    upstream.shutdown().await;
    p.shutdown().await;
}

#[tokio::test]
async fn describe_backend_lists_tools_and_argument_names_but_never_the_servers_text() {
    let up = Upstream::new(5);
    let url = start(&up).await;
    let (p, _dirs) = gateway_over(&url).await;

    let d = admin(&p, "describe_backend", json!({ "endpoint": "upstream_kb" })).await;

    let tools = d["tools"].as_array().expect("tools");
    let names: Vec<&str> = tools.iter().filter_map(|t| t["name"].as_str()).collect();
    assert_eq!(names, ["listArticles", "getArticle"], "{d}");
    let list_args: Vec<&str> = tools[0]["arguments"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|a| a["name"].as_str())
        .collect();
    assert!(
        list_args.contains(&"after") && list_args.contains(&"limit"),
        "{d}"
    );
    assert_eq!(d["trust"], "external", "{d}");
    let text = d.to_string();
    assert!(
        !text.contains(INJECTION),
        "a tool description reached the wire: {d}"
    );
    assert!(
        !text.contains("instructions"),
        "the server's instructions reached the wire: {d}"
    );

    // An endpoint that is not MCP is refused with a plain message, and an unknown one says so.
    let other = call_as(
        &p,
        Role::Admin,
        "describe_backend",
        json!({ "endpoint": "nope" }),
    )
    .await;
    assert!(other.get("error").is_some(), "{other}");
    p.shutdown().await;
}
