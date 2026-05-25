//! MCP-over-HTTP dispatcher: receives JSON-RPC 2.0 requests on
//! `POST /mcp`, routes the agent-facing read tools to `Indexer`
//! methods, returns JSON-RPC 2.0 responses.
//!
//! Wire shape follows `docs/spec/protocol.md §MCP-over-HTTP framing`
//! verbatim:
//!
//! ```jsonc
//! // request
//! { "jsonrpc": "2.0", "id": 1, "method": "tools/call",
//!   "params": { "name": "search", "arguments": { "q": "...", "k": 10 } } }
//! // response
//! { "jsonrpc": "2.0", "id": 1, "result": { ... tool output ... } }
//! // or
//! { "jsonrpc": "2.0", "id": 1, "error": { "code": -32602, "message": "..." } }
//! ```
//!
//! Today the seven read tools, `update_page`, the three live-CRDT
//! session tools (`open_session` / `apply_op` / `close_session`),
//! and the MCP `tools/list` discovery call are all wired. The
//! session tools land in M4.2 against the freshly-merged
//! `escurel-crdt` `LiveDoc` actor; their wire shape matches
//! `docs/spec/protocol.md §Write tools` verbatim. The bidi-stream
//! / WebSocket transports for the same CRDT session arrive in
//! M4.3 and M4.4 respectively.

use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use escurel_auth::{AuthContext, OidcVerifier, Role};
use escurel_crdt::{CrdtBackend, Op};
use escurel_index::{Direction, Indexer, OrderDir};
use escurel_md::PageType;
use escurel_quota::{Dimension, QuotaError, QuotaManager};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::session::{SessionError, SessionManager};

/// JSON-RPC 2.0 request envelope.
#[derive(Debug, Deserialize)]
pub struct JsonRpcRequest {
    pub jsonrpc: String,
    pub id: Value,
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

/// Inner shape of `params` for `method = "tools/call"`.
#[derive(Debug, Deserialize)]
pub struct ToolsCallParams {
    pub name: String,
    #[serde(default)]
    pub arguments: Value,
}

/// MCP entry point: `POST /mcp`. Single-request, single-response
/// for now; batched and SSE-streamed responses come with later
/// PRs.
///
/// When the gateway is configured with an [`OidcVerifier`], the
/// caller must supply `Authorization: Bearer <jwt>`; missing /
/// invalid → HTTP 401 (the JSON-RPC error envelope is only used
/// for *protocol-level* errors, per the JSON-RPC convention
/// that transport-level auth failures stay at the HTTP layer).
/// When a [`QuotaManager`] is also configured, the per-tenant
/// rate budget is debited *before* dispatch; exhaustion returns
/// HTTP 429 with a `Retry-After-Ms` header and an
/// `escurel.tool_calls{status=quota_exhausted}` semantic in the
/// body.
pub async fn mcp(
    State(state): State<crate::server::AppState>,
    headers: HeaderMap,
    Json(req): Json<JsonRpcRequest>,
) -> axum::response::Response {
    if req.jsonrpc != "2.0" {
        return error_response(req.id, -32600, "invalid jsonrpc version");
    }

    // Auth gate — only enforced when a verifier is configured.
    let auth_ctx = match state.verifier.as_ref() {
        Some(verifier) => match enforce_auth(verifier, &headers).await {
            Ok(ctx) => Some(ctx),
            Err(resp) => return resp,
        },
        None => None,
    };

    // Quota gate — only enforced when a quota manager is
    // configured (and an auth context is available to name the
    // tenant). The dimension is picked from the tool name; tools
    // that don't consume any bucket (today: tools/list and
    // `close_session`) skip the check entirely. `open_session`
    // doesn't debit a rate-limit dimension here either — it
    // acquires a `SessionGuard` from the session-cap semaphore
    // inside the tool body, so over-cap returns the
    // `session_cap_reached` JSON-RPC error rather than a
    // `429` from this middleware.
    if let (Some(quota), Some(ctx)) = (state.quota.as_ref(), auth_ctx.as_ref()) {
        if let Some(dim) = dimension_for(&req.method, &req.params) {
            if let Err(err) = quota.try_consume(&ctx.tenant_id, dim) {
                return quota_response(req.id, &err);
            }
        }
    }

    // Tenant id for tools that consume per-tenant resources
    // (session slots, in M4.2). Falls back to a deterministic
    // sentinel when no verifier is wired — dev / on-host mode.
    let tenant_id = auth_ctx
        .as_ref()
        .map(|c| c.tenant_id.clone())
        .unwrap_or_else(|| "default".to_owned());

    let result = match req.method.as_str() {
        "tools/list" => Ok(tools_list_payload()),
        "tools/call" => dispatch_tools_call(&state, &tenant_id, req.params).await,
        other => Err(JsonRpcError::method_not_found(format!(
            "unknown method `{other}`"
        ))),
    };

    match result {
        Ok(value) => (
            StatusCode::OK,
            Json(json!({
                "jsonrpc": "2.0",
                "id": req.id,
                "result": value,
            })),
        )
            .into_response(),
        Err(err) => err.into_response(req.id),
    }
}

async fn enforce_auth(
    verifier: &OidcVerifier,
    headers: &HeaderMap,
) -> Result<AuthContext, axum::response::Response> {
    let token = match bearer_token(headers) {
        Some(t) => t,
        None => return Err(auth_failure("missing Authorization: Bearer header")),
    };
    verifier
        .verify(&token)
        .await
        .map_err(|e| auth_failure(format!("token rejected: {e}")))
}

fn bearer_token(headers: &HeaderMap) -> Option<String> {
    let raw = headers.get("authorization")?.to_str().ok()?;
    let prefix = "Bearer ";
    if let Some(stripped) = raw.strip_prefix(prefix) {
        return Some(stripped.trim().to_owned());
    }
    if let Some(stripped) = raw.strip_prefix("bearer ") {
        return Some(stripped.trim().to_owned());
    }
    None
}

fn auth_failure(message: impl Into<String>) -> axum::response::Response {
    (
        StatusCode::UNAUTHORIZED,
        Json(json!({
            "error": "unauthorized",
            "message": message.into(),
        })),
    )
        .into_response()
}

fn quota_response(id: Value, err: &QuotaError) -> axum::response::Response {
    let retry = err.retry_after_ms();
    let dim = match err {
        QuotaError::Exhausted { dimension, .. } => format!("{dimension:?}").to_lowercase(),
    };
    let body = json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {
            "code": -32000,
            "message": format!("quota exhausted on {dim}; retry after {retry} ms"),
            "data": { "dimension": dim, "retry_after_ms": retry }
        }
    });
    let mut response = (StatusCode::TOO_MANY_REQUESTS, Json(body)).into_response();
    response
        .headers_mut()
        .insert("Retry-After-Ms", retry.to_string().parse().unwrap());
    response
}

/// Map (method, params) to the quota dimension a request should
/// debit, if any. Tools/list and unauthenticated discovery don't
/// consume a bucket; session-tools are special-cased.
fn dimension_for(method: &str, params: &Value) -> Option<Dimension> {
    if method != "tools/call" {
        return None;
    }
    let name = params.get("name").and_then(Value::as_str)?;
    Some(match name {
        // `apply_op` is a write; `open_session` debits a session
        // slot (semaphore, not a token bucket) inside the tool
        // body; `close_session` is a cleanup and does not debit.
        "update_page" | "apply_op" => Dimension::Writes,
        "open_session" | "close_session" => return None,
        _ => Dimension::Queries,
    })
}

#[allow(dead_code)]
fn elevated_role(role: Role) -> bool {
    matches!(role, Role::Admin)
}

async fn dispatch_tools_call(
    state: &crate::server::AppState,
    tenant_id: &str,
    params: Value,
) -> Result<Value, JsonRpcError> {
    let params: ToolsCallParams = serde_json::from_value(params)
        .map_err(|e| JsonRpcError::invalid_params(format!("tools/call params: {e}")))?;

    // Session tools depend on `crdt_backend` + `sessions`, not on
    // the indexer. Route them before the indexer gate.
    match params.name.as_str() {
        "open_session" => {
            return tool_open_session(
                state.crdt_backend.as_ref(),
                Arc::clone(&state.sessions),
                state.quota.as_ref(),
                tenant_id,
                params.arguments,
            )
            .await;
        }
        "apply_op" => {
            return tool_apply_op(
                state.crdt_backend.as_ref(),
                Arc::clone(&state.sessions),
                params.arguments,
            )
            .await;
        }
        "close_session" => {
            return tool_close_session(
                state.crdt_backend.as_ref(),
                Arc::clone(&state.sessions),
                params.arguments,
            )
            .await;
        }
        _ => {}
    }

    let indexer = state.indexer.as_ref().ok_or_else(|| {
        JsonRpcError::internal("server has no indexer wired; tools/call is unavailable")
    })?;

    match params.name.as_str() {
        "list_skills" => tool_list_skills(indexer).await,
        "list_instances" => tool_list_instances(indexer, params.arguments).await,
        "resolve" => tool_resolve(indexer, params.arguments).await,
        "expand" => tool_expand(indexer, params.arguments).await,
        "neighbours" => tool_neighbours(indexer, params.arguments).await,
        "search" => tool_search(indexer, params.arguments).await,
        "run_stored_query" => tool_run_stored_query(indexer, params.arguments).await,
        "update_page" => tool_update_page(indexer, params.arguments).await,
        other => Err(JsonRpcError::method_not_found(format!(
            "unknown tool `{other}`"
        ))),
    }
}

// --- per-tool handlers -----------------------------------------

async fn tool_list_skills(indexer: &Indexer) -> Result<Value, JsonRpcError> {
    let skills = indexer
        .list_skills()
        .await
        .map_err(|e| JsonRpcError::internal(format!("list_skills: {e}")))?;
    Ok(json!({
        "skills": skills.iter().map(|s| json!({
            "id": s.id,
            "description": s.description,
            "required_frontmatter": s.required_frontmatter,
            "optional_frontmatter": s.optional_frontmatter,
            "is_event_typed": s.is_event_typed,
        })).collect::<Vec<_>>(),
    }))
}

#[derive(Deserialize)]
struct ListInstancesArgs {
    skill_id: String,
    #[serde(default)]
    order_by: Option<String>,
    #[serde(default)]
    limit: Option<usize>,
}

async fn tool_list_instances(indexer: &Indexer, args: Value) -> Result<Value, JsonRpcError> {
    let a: ListInstancesArgs = serde_json::from_value(args)
        .map_err(|e| JsonRpcError::invalid_params(format!("list_instances: {e}")))?;
    let order = match a.order_by.as_deref() {
        Some(s) => match s.to_ascii_lowercase().as_str() {
            "at asc" | "at_asc" => Some(OrderDir::Asc),
            "at desc" | "at_desc" => Some(OrderDir::Desc),
            _ => None,
        },
        None => None,
    };
    let out = indexer
        .list_instances(&a.skill_id, order, a.limit)
        .await
        .map_err(|e| JsonRpcError::internal(format!("list_instances: {e}")))?;
    Ok(json!({
        "instances": out.iter().map(|i| json!({
            "page_id": i.page_id,
            "skill": i.skill,
            "frontmatter": i.frontmatter,
            "at": i.at,
        })).collect::<Vec<_>>(),
        "next_cursor": Value::Null,
    }))
}

#[derive(Deserialize)]
struct ResolveArgs {
    wikilink: String,
}

async fn tool_resolve(indexer: &Indexer, args: Value) -> Result<Value, JsonRpcError> {
    let a: ResolveArgs = serde_json::from_value(args)
        .map_err(|e| JsonRpcError::invalid_params(format!("resolve: {e}")))?;
    let resolved = indexer
        .resolve(&a.wikilink)
        .await
        .map_err(|e| JsonRpcError::internal(format!("resolve: {e}")))?;
    let exists = resolved.exists();
    let parsed = &resolved.parsed;
    Ok(json!({
        "parsed": {
            "skill": parsed.skill,
            "id": parsed.id,
            "anchor": parsed.anchor,
            "version": parsed.version,
            "alias": parsed.alias,
        },
        "page": resolved.page.as_ref().map(|p| json!({
            "page_id": p.page_id,
            "slug": p.slug,
            "skill": p.skill,
            "page_type": page_type_str(p.page_type),
        })),
        "exists": exists,
    }))
}

#[derive(Deserialize)]
struct ExpandArgs {
    page_id: String,
}

async fn tool_expand(indexer: &Indexer, args: Value) -> Result<Value, JsonRpcError> {
    let a: ExpandArgs = serde_json::from_value(args)
        .map_err(|e| JsonRpcError::invalid_params(format!("expand: {e}")))?;
    let out = indexer
        .expand(&a.page_id)
        .await
        .map_err(|e| JsonRpcError::internal(format!("expand: {e}")))?;
    match out {
        None => Ok(json!({ "page": Value::Null })),
        Some(e) => Ok(json!({
            "page": {
                "page_id": e.page.page_id,
                "slug": e.page.slug,
                "skill": e.page.skill,
                "page_type": page_type_str(e.page.page_type),
            },
            "frontmatter": e.frontmatter,
            "body": e.body,
            "blocks": e.blocks.iter().map(|b| json!({
                "anchor": b.anchor,
                "content": b.content,
            })).collect::<Vec<_>>(),
            "wikilinks_out": e.wikilinks_out.iter().map(|w| json!({
                "skill": w.skill, "id": w.id, "anchor": w.anchor,
                "version": w.version, "alias": w.alias,
            })).collect::<Vec<_>>(),
        })),
    }
}

#[derive(Deserialize)]
struct NeighboursArgs {
    page_id: String,
    #[serde(default)]
    direction: Option<String>,
    #[serde(default)]
    link_skill: Option<String>,
}

async fn tool_neighbours(indexer: &Indexer, args: Value) -> Result<Value, JsonRpcError> {
    let a: NeighboursArgs = serde_json::from_value(args)
        .map_err(|e| JsonRpcError::invalid_params(format!("neighbours: {e}")))?;
    let dir = match a.direction.as_deref().unwrap_or("both") {
        "in" => Direction::In,
        "out" => Direction::Out,
        "both" => Direction::Both,
        other => {
            return Err(JsonRpcError::invalid_params(format!(
                "neighbours direction `{other}`; expected in|out|both"
            )));
        }
    };
    let edges = indexer
        .neighbours(&a.page_id, dir, a.link_skill.as_deref())
        .await
        .map_err(|e| JsonRpcError::internal(format!("neighbours: {e}")))?;
    Ok(json!({
        "edges": edges.iter().map(|e| json!({
            "src_page": e.src_page,
            "dst_page": e.dst_page,
            "link_skill": e.link_skill,
            "link_version": e.link_version,
            "dst_anchor": e.dst_anchor,
        })).collect::<Vec<_>>(),
    }))
}

#[derive(Deserialize)]
struct SearchArgs {
    q: String,
    #[serde(default = "default_k")]
    k: usize,
    #[serde(default)]
    page_type: Option<String>,
    #[serde(default)]
    skill: Option<String>,
}

fn default_k() -> usize {
    10
}

async fn tool_search(indexer: &Indexer, args: Value) -> Result<Value, JsonRpcError> {
    let a: SearchArgs = serde_json::from_value(args)
        .map_err(|e| JsonRpcError::invalid_params(format!("search: {e}")))?;
    let pt = match a.page_type.as_deref() {
        None | Some("any") => None,
        Some("skill") => Some(PageType::Skill),
        Some("instance") => Some(PageType::Instance),
        Some(other) => {
            return Err(JsonRpcError::invalid_params(format!(
                "search page_type `{other}`; expected skill|instance|any"
            )));
        }
    };
    let hits = indexer
        .search(&a.q, a.k, pt, a.skill.as_deref())
        .await
        .map_err(|e| JsonRpcError::internal(format!("search: {e}")))?;
    Ok(json!({
        "hits": hits.iter().map(|h| json!({
            "page_id": h.page_id,
            "slug": h.slug,
            "skill": h.skill,
            "page_type": page_type_str(h.page_type),
            "anchor": h.anchor,
            "snippet": h.snippet,
            "score": h.score,
            "frontmatter_excerpt": h.frontmatter_excerpt,
        })).collect::<Vec<_>>(),
        "granularity": "block",
    }))
}

#[derive(Deserialize)]
struct RunStoredQueryArgs {
    query_id: String,
    #[serde(default)]
    params: serde_json::Map<String, Value>,
}

async fn tool_run_stored_query(indexer: &Indexer, args: Value) -> Result<Value, JsonRpcError> {
    let a: RunStoredQueryArgs = serde_json::from_value(args)
        .map_err(|e| JsonRpcError::invalid_params(format!("run_stored_query: {e}")))?;
    let out = indexer
        .run_stored_query(&a.query_id, &a.params)
        .await
        .map_err(|e| JsonRpcError::internal(format!("run_stored_query: {e}")))?;
    Ok(json!({
        "rows": out.rows,
        "schema": out.schema.iter().map(|c| json!({
            "name": c.name,
            "type": c.type_name,
        })).collect::<Vec<_>>(),
    }))
}

#[derive(Deserialize)]
struct UpdatePageArgs {
    page_id: String,
    content: String,
}

async fn tool_update_page(indexer: &Indexer, args: Value) -> Result<Value, JsonRpcError> {
    let a: UpdatePageArgs = serde_json::from_value(args)
        .map_err(|e| JsonRpcError::invalid_params(format!("update_page: {e}")))?;
    indexer
        .update_page(&a.page_id, &a.content)
        .await
        .map_err(|e| JsonRpcError::internal(format!("update_page: {e}")))?;
    // The trait doesn't currently surface validation issues
    // (it errors out instead). Return the protocol-required
    // `{ok, issues}` shape with an empty issues list and a stub
    // new_version derived from the body hash. Real version IDs
    // arrive once the CRDT layer (M4) gives us monotonic
    // versions; until then the client only needs the field to
    // exist.
    Ok(json!({
        "ok": true,
        "issues": [],
        "new_version": "v1",
    }))
}

// --- session tools (M4.2) --------------------------------------

#[derive(Deserialize)]
struct OpenSessionArgs {
    page_id: String,
}

async fn tool_open_session(
    backend: Option<&Arc<dyn CrdtBackend>>,
    sessions: Arc<SessionManager>,
    quota: Option<&Arc<QuotaManager>>,
    tenant_id: &str,
    args: Value,
) -> Result<Value, JsonRpcError> {
    let a: OpenSessionArgs = serde_json::from_value(args)
        .map_err(|e| JsonRpcError::invalid_params(format!("open_session: {e}")))?;
    let backend = backend
        .ok_or_else(|| JsonRpcError::internal("live CRDT mode not enabled on this server"))?;

    // Acquire a session-cap permit if quota is configured.
    // Failure → JSON-RPC `-32000` quota error (mirrors the
    // existing rate-limit response shape).
    let guard = if let Some(q) = quota {
        match q.try_acquire_session(tenant_id) {
            Some(g) => Some(g),
            None => {
                return Err(JsonRpcError {
                    code: -32000,
                    message: format!(
                        "session_cap_reached: tenant `{tenant_id}` is at its concurrent_sessions cap"
                    ),
                });
            }
        }
    } else {
        None
    };

    let (session_id, head) = sessions
        .open(Arc::clone(backend), &a.page_id, guard)
        .await
        .map_err(|e| session_error_to_jsonrpc(&e, "open_session"))?;

    Ok(json!({
        "session": session_id,
        "head_version": head.as_str(),
        // Advisory: clients with WS support should switch to the
        // WS channel after this call. The host/scheme are not
        // injected here (the gateway doesn't know its public
        // origin); the relative path is canonical.
        "ws_url": "/ws",
    }))
}

#[derive(Deserialize)]
struct ApplyOpArgs {
    session: String,
    op: String,
}

async fn tool_apply_op(
    backend: Option<&Arc<dyn CrdtBackend>>,
    sessions: Arc<SessionManager>,
    args: Value,
) -> Result<Value, JsonRpcError> {
    let a: ApplyOpArgs = serde_json::from_value(args)
        .map_err(|e| JsonRpcError::invalid_params(format!("apply_op: {e}")))?;
    if backend.is_none() {
        return Err(JsonRpcError::internal(
            "live CRDT mode not enabled on this server",
        ));
    }

    let op_bytes = B64
        .decode(a.op.as_bytes())
        .map_err(|e| JsonRpcError::invalid_params(format!("apply_op `op` is not base64: {e}")))?;
    let merged = sessions
        .apply(&a.session, Op::new(op_bytes))
        .await
        .map_err(|e| session_error_to_jsonrpc(&e, "apply_op"))?;
    Ok(json!({
        "ok": true,
        "merged_version": merged.as_str(),
    }))
}

#[derive(Deserialize)]
struct CloseSessionArgs {
    session: String,
    #[serde(default = "default_commit")]
    commit: bool,
}

fn default_commit() -> bool {
    true
}

async fn tool_close_session(
    backend: Option<&Arc<dyn CrdtBackend>>,
    sessions: Arc<SessionManager>,
    args: Value,
) -> Result<Value, JsonRpcError> {
    let a: CloseSessionArgs = serde_json::from_value(args)
        .map_err(|e| JsonRpcError::invalid_params(format!("close_session: {e}")))?;
    if backend.is_none() {
        return Err(JsonRpcError::internal(
            "live CRDT mode not enabled on this server",
        ));
    }
    let final_v = sessions
        .close(&a.session, a.commit)
        .await
        .map_err(|e| session_error_to_jsonrpc(&e, "close_session"))?;
    Ok(json!({
        "ok": true,
        "final_version": final_v.as_str(),
        "issues": [],
    }))
}

/// Map a [`SessionError`] to the JSON-RPC error envelope.
/// `UnknownSession` and the underlying LiveDoc errors both surface
/// as `-32603 internal` per the spec (the wire shape doesn't
/// have a distinct "not found" code for tools).
fn session_error_to_jsonrpc(err: &SessionError, tool: &str) -> JsonRpcError {
    JsonRpcError::internal(format!("{tool}: {err}"))
}

// --- tools/list payload ----------------------------------------

/// MCP `tools/list` response payload. Each entry is `{ name,
/// description, inputSchema }`. The wire shape matches the
/// upstream MCP spec exactly so any conforming MCP client can
/// drive Escurel without bespoke wiring.
fn tools_list_payload() -> Value {
    json!({
        "tools": [
            tool_entry(
                "list_skills",
                "Return the tenant's Tier-1 skill catalogue.",
                json!({ "type": "object", "properties": {} }),
            ),
            tool_entry(
                "list_instances",
                "Enumerate instances of a skill.",
                json!({
                    "type": "object",
                    "required": ["skill_id"],
                    "properties": {
                        "skill_id": { "type": "string" },
                        "order_by": { "type": "string", "enum": ["at asc", "at desc"] },
                        "limit": { "type": "integer", "minimum": 1, "maximum": 10000 }
                    }
                }),
            ),
            tool_entry(
                "resolve",
                "Parse a [[wikilink]] and look up its target page.",
                json!({
                    "type": "object",
                    "required": ["wikilink"],
                    "properties": { "wikilink": { "type": "string" } }
                }),
            ),
            tool_entry(
                "expand",
                "Fetch a page's frontmatter + body + outbound wikilinks.",
                json!({
                    "type": "object",
                    "required": ["page_id"],
                    "properties": { "page_id": { "type": "string" } }
                }),
            ),
            tool_entry(
                "neighbours",
                "Typed link-graph traversal.",
                json!({
                    "type": "object",
                    "required": ["page_id"],
                    "properties": {
                        "page_id": { "type": "string" },
                        "direction": { "type": "string", "enum": ["in", "out", "both"] },
                        "link_skill": { "type": "string" }
                    }
                }),
            ),
            tool_entry(
                "search",
                "Hybrid vector + FTS search, RRF-fused.",
                json!({
                    "type": "object",
                    "required": ["q"],
                    "properties": {
                        "q": { "type": "string" },
                        "k": { "type": "integer", "minimum": 0, "maximum": 1000 },
                        "page_type": { "type": "string", "enum": ["skill", "instance", "any"] },
                        "skill": { "type": "string" }
                    }
                }),
            ),
            tool_entry(
                "run_stored_query",
                "Execute a [[query::*]] instance with named parameters.",
                json!({
                    "type": "object",
                    "required": ["query_id"],
                    "properties": {
                        "query_id": { "type": "string" },
                        "params": { "type": "object" }
                    }
                }),
            ),
            tool_entry(
                "update_page",
                "Upsert a markdown page (whole-body write).",
                json!({
                    "type": "object",
                    "required": ["page_id", "content"],
                    "properties": {
                        "page_id": { "type": "string" },
                        "content": { "type": "string" }
                    }
                }),
            ),
            tool_entry(
                "open_session",
                "Open a live CRDT session on a page; returns a session id and the WS upgrade URL.",
                json!({
                    "type": "object",
                    "required": ["page_id"],
                    "properties": {
                        "page_id": { "type": "string" }
                    }
                }),
            ),
            tool_entry(
                "apply_op",
                "Apply a base64-encoded Loro op blob to an open session.",
                json!({
                    "type": "object",
                    "required": ["session", "op"],
                    "properties": {
                        "session": { "type": "string" },
                        "op": { "type": "string", "description": "base64-encoded Loro op bytes" }
                    }
                }),
            ),
            tool_entry(
                "close_session",
                "Close a session; optionally snapshot the doc (commit=true).",
                json!({
                    "type": "object",
                    "required": ["session"],
                    "properties": {
                        "session": { "type": "string" },
                        "commit": { "type": "boolean", "default": true }
                    }
                }),
            ),
        ]
    })
}

fn tool_entry(name: &str, description: &str, input_schema: Value) -> Value {
    json!({
        "name": name,
        "description": description,
        "inputSchema": input_schema,
    })
}

// --- helpers ---------------------------------------------------

fn page_type_str(pt: PageType) -> &'static str {
    match pt {
        PageType::Skill => "skill",
        PageType::Instance => "instance",
    }
}

#[derive(Debug)]
struct JsonRpcError {
    code: i32,
    message: String,
}

impl JsonRpcError {
    fn method_not_found(msg: impl Into<String>) -> Self {
        Self {
            code: -32601,
            message: msg.into(),
        }
    }
    fn invalid_params(msg: impl Into<String>) -> Self {
        Self {
            code: -32602,
            message: msg.into(),
        }
    }
    fn internal(msg: impl Into<String>) -> Self {
        Self {
            code: -32603,
            message: msg.into(),
        }
    }
    fn into_response(self, id: Value) -> axum::response::Response {
        (
            StatusCode::OK,
            Json(json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": { "code": self.code, "message": self.message },
            })),
        )
            .into_response()
    }
}

fn error_response(id: Value, code: i32, msg: impl Into<String>) -> axum::response::Response {
    JsonRpcError {
        code,
        message: msg.into(),
    }
    .into_response(id)
}
