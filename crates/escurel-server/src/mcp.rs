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
use escurel_admin::{TenantSpec as AdminTenantSpec, TenantStore, validate_tenant_id};
use escurel_auth::{AuthContext, OidcVerifier, Role};
use escurel_crdt::{CrdtBackend, Op};
use escurel_index::{
    AppendChatMessage, ChatMessage, Direction, EventInfo, Granularity, Indexer, IndexerError,
    Issue, ListChatMessages, NewEvent, OrderDir, Severity, is_safe_attach_source,
};
use escurel_md::PageType;
use escurel_quota::{Dimension, QuotaError, QuotaManager};
use escurel_storage::{Key, StoreError};
use escurel_types::{
    AdminLaneBlobResponse, AttachExternalResponse, CompactProgress, EmbeddingReloadResponse,
    ListSkillsResponse, QuotaGetResponse, RebuildProgress, Skill as TypesSkill,
    TenantCreateResponse, TenantDeleteResponse, TenantGetResponse, TenantImportResponse,
    TenantListResponse, TenantSpec as TypesTenantSpec, TenantUpdateResponse,
};
use serde::Deserialize;
use serde_json::{Value, json};
use tracing::Instrument;

use crate::server::AppState;
use crate::session::{SessionError, SessionManager};
use crate::tenant_archive::{tar_gz_into_chunks, untar_gz_into};

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
    // Every accepted /mcp request bumps the request counter so the
    // /metrics scrape reflects real traffic. Status is recorded as
    // 200 here (the JSON-RPC envelope carries any error inside a
    // 200 body); transport-level failures (auth 401, quota 429)
    // are bumped separately at their own return points.
    state.metrics.inc_request("/mcp", 200);

    // Per-request span: every record emitted while the dispatcher
    // runs carries `request_id` + `method` + `tool` (when
    // applicable) hoisted to the top level by escurel-obs's JSON
    // formatter. Substrate audit collectors key off `request_id`,
    // and the operator dashboards group by `tool`. We instrument
    // an inner async block (not `span.enter()`) so the span guard
    // doesn't cross an `.await` — the classic async-tracing
    // footgun where a thread-local guard leaks into the next
    // poll's task.
    let request_id = request_id_from(&headers);
    let tool_name = tool_name_from(&req.method, &req.params).unwrap_or_default();
    // Per-record audit fields per `platform.md §Observability`:
    // `transport` + `trace_id` are known up front; `tenant` + `subject`
    // are filled in (`span.record`) once auth resolves. The JSON
    // formatter hoists all span fields onto every record emitted
    // inside the span, so the `tool.completed` event below carries the
    // full contract set (tenant/tool/transport/subject/trace_id/
    // duration_ms). `trace_id` mirrors the gateway `request_id` when no
    // OTel trace context is active.
    let span = tracing::info_span!(
        "mcp.request",
        request_id = %request_id,
        trace_id = %request_id,
        transport = "mcp_http",
        method = %req.method,
        tool = %tool_name,
    );
    mcp_inner(state, headers, req).instrument(span).await
}

async fn mcp_inner(
    state: crate::server::AppState,
    headers: HeaderMap,
    req: JsonRpcRequest,
) -> axum::response::Response {
    tracing::info!(msg = "mcp.request.start", "mcp.request.start");

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
    if let (Some(quota), Some(ctx)) = (state.quota.as_ref(), auth_ctx.as_ref())
        && let Some(dim) = dimension_for(&req.method, &req.params)
        && let Err(err) = quota.try_consume(&ctx.tenant_id, dim)
    {
        return quota_response(req.id, &err);
    }

    // Tenant id for tools that consume per-tenant resources
    // (session slots, in M4.2). Falls back to a deterministic
    // sentinel when no verifier is wired — dev / on-host mode.
    let tenant_id = auth_ctx
        .as_ref()
        .map(|c| c.tenant_id.clone())
        .unwrap_or_else(|| "default".to_owned());

    // Caller role for the admin-tool gate. `None` when no verifier
    // is wired (dev / on-host mode) — the gateway is open, so admin
    // tools are allowed (the local demo runs without a token).
    let role = auth_ctx.as_ref().map(|c| c.role);

    // Auth-derived audit fields for the `tool.completed` record.
    // `subject` is the token `sub` claim; `anonymous` in
    // unauthenticated dev mode.
    let subject = auth_ctx
        .as_ref()
        .map(|c| c.subject.clone())
        .unwrap_or_else(|| "anonymous".to_owned());

    let result = match req.method.as_str() {
        "tools/list" => Ok(tools_list_payload()),
        "tools/call" => {
            // Per-tool metrics (escurel_tool_calls / _latency_ms):
            // name the tool, time the dispatch, record on completion.
            let tool = req
                .params
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            let started = std::time::Instant::now();
            let r = dispatch_tools_call(&state, &tenant_id, role, req.params).await;
            let status = if r.is_ok() { "ok" } else { "error" };
            let duration_ms = started.elapsed().as_secs_f64() * 1000.0;
            state
                .metrics
                .record_tool_call(&tenant_id, &tool, "mcp_http", status, duration_ms);
            // Audit record carrying the full per-record contract set
            // (platform.md §Observability). transport/trace_id/request_id
            // are hoisted from the span; tenant/subject/tool/duration are
            // on the event (the obs layer captures span fields at
            // creation, so auth-derived values must ride the event).
            tracing::info!(
                tenant = %tenant_id,
                subject = %subject,
                tool = %tool,
                status,
                duration_ms,
                msg = "tool.completed",
                "tool.completed"
            );
            r
        }
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

/// Read `X-Request-Id` from `headers` if present and non-empty;
/// otherwise mint a fresh ULID. Substrate audit collectors key
/// off `request_id`, and tests pin a known value through the
/// header to assert end-to-end propagation.
fn request_id_from(headers: &HeaderMap) -> String {
    if let Some(raw) = headers.get("x-request-id").and_then(|v| v.to_str().ok()) {
        let trimmed = raw.trim();
        if !trimmed.is_empty() {
            return trimmed.to_owned();
        }
    }
    ulid::Ulid::new().to_string()
}

/// Extract the tool name from a JSON-RPC `tools/call` request so
/// we can stamp it on the request span. Returns `None` for other
/// methods (e.g. `tools/list`); the span then carries an empty
/// `tool` field rather than `Optional`.
fn tool_name_from(method: &str, params: &Value) -> Option<String> {
    if method != "tools/call" {
        return None;
    }
    params
        .get("name")
        .and_then(Value::as_str)
        .map(str::to_owned)
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
    // Admin / operator tools are role-gated, not part of the tenant's
    // *agent* rate budget — they must not debit the query/write
    // buckets (the old gRPC admin surface carried no quota
    // middleware). Otherwise an operator's own `admin_quota` snapshot
    // would read back one-less-than-full.
    if name.starts_with("admin_")
        || name.starts_with("tenant_")
        || matches!(
            name,
            "rebuild" | "compact_lanes" | "attach_external" | "embedding_reload"
        )
    {
        return None;
    }
    Some(match name {
        // `apply_op` is a write; `open_session` debits a session
        // slot (semaphore, not a token bucket) inside the tool
        // body; `close_session` is a cleanup and does not debit.
        "update_page" | "apply_op" | "append_message" | "capture_event" | "assign_event" => {
            Dimension::Writes
        }
        "open_session" | "close_session" => return None,
        _ => Dimension::Queries,
    })
}

/// Gate the admin-only MCP tools. The caller's `role` is `None` only
/// when no OIDC verifier is wired (dev / on-host mode), in which case
/// the gateway is unauthenticated and everything — including the
/// admin tools — is open, so the local demo works without a token.
/// When a verifier *is* configured, the JWT must carry the admin
/// role; an agent-role caller gets a JSON-RPC error (it never reveals
/// more than "admin role required").
fn require_admin(role: Option<Role>) -> Result<(), JsonRpcError> {
    match role {
        None | Some(Role::Admin) => Ok(()),
        Some(_) => Err(JsonRpcError {
            code: -32001,
            message: "admin role required for this tool".to_owned(),
        }),
    }
}

/// Serialize an `escurel_types` response struct to a JSON-RPC result
/// value. The escurel-types structs are the wire contract; a
/// serialization failure here is a server bug, surfaced as internal.
fn to_value<T: serde::Serialize>(resp: T) -> Result<Value, JsonRpcError> {
    serde_json::to_value(resp)
        .map_err(|e| JsonRpcError::internal(format!("serialize response: {e}")))
}

async fn dispatch_tools_call(
    state: &crate::server::AppState,
    tenant_id: &str,
    role: Option<Role>,
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
        // Admin-gated tenant CRUD + long-running ops. These take
        // `state` directly (tenant_store / indexer / crdt_backend /
        // embedder seam) rather than the bound indexer, so they route
        // before the indexer gate, mirroring the session tools above.
        "tenant_create" => {
            require_admin(role)?;
            return tool_tenant_create(state, params.arguments).await;
        }
        "tenant_list" => {
            require_admin(role)?;
            return tool_tenant_list(state).await;
        }
        "tenant_get" => {
            require_admin(role)?;
            return tool_tenant_get(state, params.arguments).await;
        }
        "tenant_update" => {
            require_admin(role)?;
            return tool_tenant_update(state, params.arguments).await;
        }
        "tenant_delete" => {
            require_admin(role)?;
            return tool_tenant_delete(state, params.arguments).await;
        }
        "tenant_export" => {
            require_admin(role)?;
            return tool_tenant_export(state, params.arguments).await;
        }
        "tenant_import" => {
            require_admin(role)?;
            return tool_tenant_import(state, params.arguments).await;
        }
        "attach_external" => {
            require_admin(role)?;
            return tool_attach_external(state, params.arguments).await;
        }
        "embedding_reload" => {
            require_admin(role)?;
            return tool_embedding_reload(state).await;
        }
        "rebuild" => {
            require_admin(role)?;
            return tool_rebuild(state, params.arguments).await;
        }
        "compact_lanes" => {
            require_admin(role)?;
            return tool_compact_lanes(state, params.arguments).await;
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
        "validate" => tool_validate(indexer, params.arguments).await,
        "update_page" => tool_update_page(indexer, params.arguments).await,
        "append_message" => tool_append_message(indexer, params.arguments).await,
        "list_messages" => tool_list_messages(indexer, params.arguments).await,
        "capture_event" => {
            tool_capture_event(indexer, state.webhook.as_ref(), params.arguments).await
        }
        "list_inbox" => tool_list_inbox(indexer, params.arguments).await,
        "list_events" => tool_list_events(indexer, params.arguments).await,
        "list_snapshots" => tool_list_snapshots(indexer, params.arguments).await,
        "assign_event" => tool_assign_event(indexer, params.arguments).await,
        // Admin-gated ops tools (mirror the documented MCP admin
        // surface; delegate to the same logic as EscurelAdmin gRPC).
        "admin_quota" => {
            require_admin(role)?;
            tool_admin_quota(state, tenant_id)
        }
        "admin_audit" => {
            require_admin(role)?;
            tool_admin_audit(indexer).await
        }
        "admin_index_query" => {
            require_admin(role)?;
            tool_admin_index_query(indexer, params.arguments).await
        }
        "admin_delete_chat_history" => {
            require_admin(role)?;
            tool_admin_delete_chat_history(indexer, params.arguments).await
        }
        "admin_list_lanes" => {
            require_admin(role)?;
            tool_admin_list_lanes(indexer)
        }
        "admin_lane_keys" => {
            require_admin(role)?;
            tool_admin_lane_keys(indexer, params.arguments).await
        }
        "admin_lane_blob" => {
            require_admin(role)?;
            tool_admin_lane_blob(indexer, params.arguments).await
        }
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
    let resp = ListSkillsResponse {
        skills: skills
            .into_iter()
            .map(|s| TypesSkill {
                id: s.id,
                description: s.description,
                required_frontmatter: s.required_frontmatter,
                optional_frontmatter: s.optional_frontmatter,
                is_event_typed: s.is_event_typed,
            })
            .collect(),
    };
    to_value(resp)
}

#[derive(Deserialize)]
struct ListInstancesArgs {
    skill_id: String,
    #[serde(default)]
    order_by: Option<String>,
    #[serde(default)]
    limit: Option<usize>,
    /// Optional single frontmatter equality filter, e.g.
    /// `{"frontmatter_key": "source", "frontmatter_value": "gmail"}`
    /// for the source-inbox view. Both must be present to apply.
    #[serde(default)]
    frontmatter_key: Option<String>,
    #[serde(default)]
    frontmatter_value: Option<String>,
    /// RFC 3339 time-travel cut; instances born after it are excluded
    /// (untimed instances always remain).
    #[serde(default)]
    as_of: Option<String>,
    /// Scenario overlay; null/absent = base only, else base ∪ overlay
    /// with the overlay winning per slug.
    #[serde(default)]
    scenario: Option<String>,
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
    let filter = match (a.frontmatter_key.as_deref(), a.frontmatter_value.as_deref()) {
        (Some(k), Some(v)) if !k.is_empty() => Some((k, v)),
        _ => None,
    };
    let out = indexer
        .list_instances(
            &a.skill_id,
            order,
            a.limit,
            filter,
            a.as_of.as_deref(),
            a.scenario.as_deref(),
        )
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
    /// Scenario overlay to resolve against; null/absent = base only.
    #[serde(default)]
    scenario: Option<String>,
}

async fn tool_resolve(indexer: &Indexer, args: Value) -> Result<Value, JsonRpcError> {
    let a: ResolveArgs = serde_json::from_value(args)
        .map_err(|e| JsonRpcError::invalid_params(format!("resolve: {e}")))?;
    let resolved = indexer
        .resolve(&a.wikilink, a.scenario.as_deref())
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
    /// RFC 3339 time-travel cut; the page resolves to null if born after it.
    #[serde(default)]
    as_of: Option<String>,
    /// Scenario overlay to read against; null/absent = base only.
    #[serde(default)]
    scenario: Option<String>,
}

async fn tool_expand(indexer: &Indexer, args: Value) -> Result<Value, JsonRpcError> {
    let a: ExpandArgs = serde_json::from_value(args)
        .map_err(|e| JsonRpcError::invalid_params(format!("expand: {e}")))?;
    let out = indexer
        .expand(&a.page_id, a.as_of.as_deref(), a.scenario.as_deref())
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
    /// RFC 3339 time-travel cut; edges from sources born after it are hidden.
    #[serde(default)]
    as_of: Option<String>,
    /// Scenario overlay; edges are filtered by their source page's scenario.
    #[serde(default)]
    scenario: Option<String>,
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
        .neighbours(
            &a.page_id,
            dir,
            a.link_skill.as_deref(),
            a.as_of.as_deref(),
            a.scenario.as_deref(),
        )
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
    /// RFC 3339 time-travel cut; blocks born after it are excluded.
    #[serde(default)]
    as_of: Option<String>,
    /// Scenario overlay; base-only when null/absent.
    #[serde(default)]
    scenario: Option<String>,
    /// `"block"` (default) or `"page"`.
    #[serde(default)]
    granularity: Option<String>,
    /// Frontmatter post-filter object (see `escurel_index::filter`).
    #[serde(default)]
    filter: Option<Value>,
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
    let granularity = Granularity::from_arg(a.granularity.as_deref().unwrap_or_default());
    // An empty `{}` filter is treated as "no filter".
    let filter = a.filter.as_ref().filter(|f| !is_empty_filter(f));
    let hits = indexer
        .search_with(
            &a.q,
            a.k,
            pt,
            a.skill.as_deref(),
            a.as_of.as_deref(),
            a.scenario.as_deref(),
            granularity,
            filter,
        )
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
        "granularity": granularity.as_str(),
    }))
}

/// True for `null` or an empty `{}` filter object — both mean "no
/// post-filter", so we skip the work and the `Some`/`None` plumbing.
fn is_empty_filter(f: &Value) -> bool {
    f.is_null() || f.as_object().is_some_and(serde_json::Map::is_empty)
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
struct ValidateArgs {
    content: String,
    #[serde(default)]
    as_page_id: Option<String>,
}

async fn tool_validate(indexer: &Indexer, args: Value) -> Result<Value, JsonRpcError> {
    let a: ValidateArgs = serde_json::from_value(args)
        .map_err(|e| JsonRpcError::invalid_params(format!("validate: {e}")))?;
    let issues = indexer
        .validate(a.as_page_id.as_deref(), &a.content)
        .await
        .map_err(|e| JsonRpcError::internal(format!("validate: {e}")))?;
    // `ok` is false iff any issue is error-severity, mirroring the
    // documented ValidateResponse contract (warnings/infos don't fail
    // a draft). The wire carries both `ok` and the full issue list.
    let ok = !issues.iter().any(|i| i.severity == Severity::Error);
    Ok(json!({
        "ok": ok,
        "issues": issues.iter().map(issue_to_json).collect::<Vec<_>>(),
    }))
}

/// Map a domain [`Issue`] to the MCP JSON shape from
/// `docs/spec/protocol.md §Issue`.
fn issue_to_json(issue: &Issue) -> Value {
    let mut obj = json!({
        "severity": issue.severity.as_str(),
        "code": issue.code,
        "location": issue.location,
        "message": issue.message,
    });
    if let Some(s) = &issue.suggestion {
        obj["suggestion"] = json!(s);
    }
    obj
}

#[derive(Deserialize)]
struct UpdatePageArgs {
    page_id: String,
    content: String,
}

async fn tool_update_page(indexer: &Indexer, args: Value) -> Result<Value, JsonRpcError> {
    let a: UpdatePageArgs = serde_json::from_value(args)
        .map_err(|e| JsonRpcError::invalid_params(format!("update_page: {e}")))?;
    match indexer.update_page(&a.page_id, &a.content).await {
        // The trait doesn't yet surface non-fatal validation issues
        // (M4); return the protocol `{ok, issues}` shape with an empty
        // list and a stub `new_version` until monotonic CRDT versions
        // land.
        Ok(()) => Ok(json!({
            "ok": true,
            "issues": [],
            "new_version": "v1",
        })),
        // The protected meta-skill rejects the write as an
        // error-severity issue rather than an opaque server error.
        Err(IndexerError::MetaSkillProtected { reason }) => Ok(json!({
            "ok": false,
            "issues": [{
                "severity": "error",
                "code": "meta_skill_protected",
                "location": "frontmatter",
                "message": reason,
            }],
        })),
        Err(e) => Err(JsonRpcError::internal(format!("update_page: {e}"))),
    }
}

// --- chat tools (M-Chat, issue #63) -----------------------------

#[derive(Deserialize)]
struct AppendMessageArgs {
    chat_group_id: String,
    role: String,
    content: String,
    #[serde(default)]
    author: Option<String>,
    #[serde(default)]
    ts: Option<String>,
    #[serde(default)]
    metadata: Option<Value>,
    #[serde(default)]
    msg_id: Option<String>,
    #[serde(default = "default_embed")]
    embed: bool,
}

fn default_embed() -> bool {
    true
}

async fn tool_append_message(indexer: &Indexer, args: Value) -> Result<Value, JsonRpcError> {
    let a: AppendMessageArgs = serde_json::from_value(args)
        .map_err(|e| JsonRpcError::invalid_params(format!("append_message: {e}")))?;
    let stored = indexer
        .append_chat_message(AppendChatMessage {
            chat_group_id: &a.chat_group_id,
            role: &a.role,
            content: &a.content,
            author: a.author.as_deref(),
            ts: a.ts.as_deref(),
            metadata: a.metadata,
            msg_id: a.msg_id.as_deref(),
            embed: a.embed,
        })
        .await
        .map_err(|e| JsonRpcError::internal(format!("append_message: {e}")))?;
    Ok(json!({
        "msg_id": stored.msg_id,
        "ts": stored.ts,
    }))
}

#[derive(Deserialize)]
struct ListMessagesArgs {
    chat_group_id: String,
    #[serde(default)]
    since: Option<String>,
    #[serde(default)]
    until: Option<String>,
    #[serde(default = "default_chat_limit")]
    limit: usize,
    #[serde(default)]
    cursor: Option<String>,
    #[serde(default)]
    direction: Option<String>,
}

fn default_chat_limit() -> usize {
    100
}

async fn tool_list_messages(indexer: &Indexer, args: Value) -> Result<Value, JsonRpcError> {
    let a: ListMessagesArgs = serde_json::from_value(args)
        .map_err(|e| JsonRpcError::invalid_params(format!("list_messages: {e}")))?;
    // Default to descending — typical "give me the most recent N"
    // call site. Consumers paging the forward log pass "asc".
    let direction = match a
        .direction
        .as_deref()
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        None | Some("") | Some("desc") => OrderDir::Desc,
        Some("asc") => OrderDir::Asc,
        Some(other) => {
            return Err(JsonRpcError::invalid_params(format!(
                "list_messages: direction `{other}`; expected asc|desc",
            )));
        }
    };
    let page = indexer
        .list_chat_messages(ListChatMessages {
            chat_group_id: &a.chat_group_id,
            since: a.since.as_deref(),
            until: a.until.as_deref(),
            limit: a.limit,
            cursor: a.cursor.as_deref(),
            direction,
        })
        .await
        .map_err(|e| JsonRpcError::internal(format!("list_messages: {e}")))?;
    let messages: Vec<Value> = page.messages.iter().map(chat_message_to_json).collect();
    let mut out = json!({ "messages": messages });
    if let Some(c) = page.next_cursor {
        out["next_cursor"] = json!(c);
    }
    Ok(out)
}

fn chat_message_to_json(m: &ChatMessage) -> Value {
    let mut out = json!({
        "chat_group_id": m.chat_group_id,
        "msg_id": m.msg_id,
        "ts": m.ts,
        "role": m.role,
        "content": m.content,
        "embedded": m.embedded,
    });
    if let Some(author) = &m.author {
        out["author"] = json!(author);
    }
    if let Some(meta) = &m.metadata {
        out["metadata"] = meta.clone();
    }
    out
}

// --- events / inbox tools (M7 — Event-sourcing surface) --------

#[derive(Deserialize)]
struct CaptureEventArgs {
    #[serde(default)]
    event_id: Option<String>,
    #[serde(default)]
    at: Option<String>,
    #[serde(default)]
    source: String,
    #[serde(default)]
    mime: String,
    /// Skill id that knows how to process this event type (the label→skill link).
    #[serde(default)]
    label_skill: String,
    /// Optional candidate instance (Gmail-label style); the event still
    /// lands in the inbox until `assign_event`.
    #[serde(default)]
    instance_page_id: Option<String>,
    #[serde(default)]
    title: String,
    #[serde(default)]
    body: String,
    #[serde(default)]
    provenance: Option<Value>,
}

async fn tool_capture_event(
    indexer: &Indexer,
    webhook: Option<&crate::webhook::Webhook>,
    args: Value,
) -> Result<Value, JsonRpcError> {
    let a: CaptureEventArgs = serde_json::from_value(args)
        .map_err(|e| JsonRpcError::invalid_params(format!("capture_event: {e}")))?;
    let stored = indexer
        .capture_event(NewEvent {
            event_id: a.event_id,
            at: a.at,
            source: a.source,
            mime: a.mime,
            label_skill: a.label_skill,
            instance_page_id: a.instance_page_id,
            title: a.title,
            body: a.body,
            provenance: a.provenance,
        })
        .await
        .map_err(|e| JsonRpcError::internal(format!("capture_event: {e}")))?;
    let event = event_to_json(&stored);
    // Notify any external processor of the new inbox item (opt-in,
    // fire-and-forget; never fails the capture).
    if let Some(hook) = webhook {
        hook.notify(event.clone());
    }
    Ok(event)
}

#[derive(Deserialize)]
struct ListInboxArgs {
    #[serde(default)]
    limit: Option<usize>,
}

async fn tool_list_inbox(indexer: &Indexer, args: Value) -> Result<Value, JsonRpcError> {
    let a: ListInboxArgs = serde_json::from_value(args)
        .map_err(|e| JsonRpcError::invalid_params(format!("list_inbox: {e}")))?;
    let events = indexer
        .list_inbox(a.limit)
        .await
        .map_err(|e| JsonRpcError::internal(format!("list_inbox: {e}")))?;
    Ok(json!({ "events": events.iter().map(event_to_json).collect::<Vec<_>>() }))
}

#[derive(Deserialize)]
struct ListEventsArgs {
    instance_page_id: String,
    #[serde(default)]
    limit: Option<usize>,
}

async fn tool_list_events(indexer: &Indexer, args: Value) -> Result<Value, JsonRpcError> {
    let a: ListEventsArgs = serde_json::from_value(args)
        .map_err(|e| JsonRpcError::invalid_params(format!("list_events: {e}")))?;
    let events = indexer
        .list_events(&a.instance_page_id, a.limit)
        .await
        .map_err(|e| JsonRpcError::internal(format!("list_events: {e}")))?;
    Ok(json!({ "events": events.iter().map(event_to_json).collect::<Vec<_>>() }))
}

#[derive(Deserialize)]
struct ListSnapshotsArgs {
    page_id: String,
}

async fn tool_list_snapshots(indexer: &Indexer, args: Value) -> Result<Value, JsonRpcError> {
    let a: ListSnapshotsArgs = serde_json::from_value(args)
        .map_err(|e| JsonRpcError::invalid_params(format!("list_snapshots: {e}")))?;
    let snapshots = indexer
        .list_snapshots(&a.page_id)
        .await
        .map_err(|e| JsonRpcError::internal(format!("list_snapshots: {e}")))?;
    Ok(json!({ "snapshots": snapshots }))
}

#[derive(Deserialize)]
struct AssignEventArgs {
    event_id: String,
    instance_page_id: String,
}

async fn tool_assign_event(indexer: &Indexer, args: Value) -> Result<Value, JsonRpcError> {
    let a: AssignEventArgs = serde_json::from_value(args)
        .map_err(|e| JsonRpcError::invalid_params(format!("assign_event: {e}")))?;
    indexer
        .assign_event(&a.event_id, &a.instance_page_id)
        .await
        .map_err(|e| JsonRpcError::internal(format!("assign_event: {e}")))?;
    Ok(
        json!({ "event_id": a.event_id, "instance_page_id": a.instance_page_id, "status": "processed" }),
    )
}

fn event_to_json(e: &EventInfo) -> Value {
    json!({
        "event_id": e.event_id,
        "at": e.at,
        "source": e.source,
        "mime": e.mime,
        "label_skill": e.label_skill,
        "instance_page_id": e.instance_page_id,
        "status": e.status,
        "title": e.title,
        "body": e.body,
        "provenance": e.provenance,
    })
}

// --- admin ops tools (admin-role gated) ------------------------
//
// These mirror the documented MCP admin surface and delegate to the
// same logic the gRPC `EscurelAdmin` service uses. The role gate is
// applied by the dispatcher (`require_admin`) before these run.

fn tool_admin_quota(
    state: &crate::server::AppState,
    tenant_id: &str,
) -> Result<Value, JsonRpcError> {
    let quota = state
        .quota
        .as_ref()
        .ok_or_else(|| JsonRpcError::internal("no quota manager wired on this server"))?;
    let s = quota.snapshot(tenant_id);
    to_value(QuotaGetResponse {
        queries_remaining: s.queries_remaining,
        writes_remaining: s.writes_remaining,
        embeds_remaining: s.embeds_remaining,
        concurrent_sessions: s.concurrent_sessions_in_use,
    })
}

async fn tool_admin_audit(indexer: &Indexer) -> Result<Value, JsonRpcError> {
    let drift = indexer
        .audit()
        .await
        .map_err(|e| JsonRpcError::internal(format!("admin_audit: {e}")))?;
    Ok(json!({
        "markdown_not_in_duckdb": drift.markdown_not_in_duckdb,
        "indexed_but_no_markdown": drift.indexed_but_no_markdown,
    }))
}

#[derive(Deserialize)]
struct AdminIndexQueryArgs {
    table: String,
    #[serde(default = "default_inspect_limit")]
    limit: usize,
}

fn default_inspect_limit() -> usize {
    100
}

async fn tool_admin_index_query(indexer: &Indexer, args: Value) -> Result<Value, JsonRpcError> {
    let a: AdminIndexQueryArgs = serde_json::from_value(args)
        .map_err(|e| JsonRpcError::invalid_params(format!("admin_index_query: {e}")))?;
    let res = indexer
        .inspect_table(&a.table, a.limit)
        .await
        // Unknown-table / bad-arg errors are caller errors, not server
        // faults — surface as invalid_params.
        .map_err(|e| JsonRpcError::invalid_params(format!("admin_index_query: {e}")))?;
    Ok(json!({
        "rows": res.rows,
        "schema": res.schema.iter().map(|c| json!({
            "name": c.name,
            "type": c.type_name,
        })).collect::<Vec<_>>(),
    }))
}

// --- admin lane introspection (mirrors EscurelAdmin gRPC) ---------

/// Canonical (and only) lane this server exposes.
const LANE_NAME: &str = "markdown";
/// Hard cap on a single `admin_lane_blob` transfer (1 MiB).
const LANE_BLOB_MAX_BYTES: u64 = 1024 * 1024;

fn lane_name_ok(lane: &str) -> Result<(), JsonRpcError> {
    if lane.is_empty() || lane == LANE_NAME {
        Ok(())
    } else {
        Err(JsonRpcError::invalid_params(format!(
            "unknown lane `{lane}`; this server exposes only `{LANE_NAME}`"
        )))
    }
}

fn lane_content_type(key: &str) -> &'static str {
    if key.ends_with(".md") {
        "text/markdown"
    } else if key.ends_with(".json") {
        "application/json"
    } else {
        "application/octet-stream"
    }
}

fn tool_admin_list_lanes(indexer: &Indexer) -> Result<Value, JsonRpcError> {
    Ok(json!({
        "lanes": [{
            "name": LANE_NAME,
            "backend": indexer.lane_store().backend(),
            "tenants_present": [indexer.tenant()],
        }],
    }))
}

#[derive(Deserialize)]
struct AdminLaneKeysArgs {
    #[serde(default)]
    lane: String,
    #[serde(default)]
    prefix: String,
    #[serde(default)]
    limit: usize,
}

async fn tool_admin_lane_keys(indexer: &Indexer, args: Value) -> Result<Value, JsonRpcError> {
    let a: AdminLaneKeysArgs = serde_json::from_value(args)
        .map_err(|e| JsonRpcError::invalid_params(format!("admin_lane_keys: {e}")))?;
    lane_name_ok(&a.lane)?;
    let store = indexer.lane_store();
    let prefix = Key::new(indexer.tenant(), a.prefix)
        .map_err(|e| JsonRpcError::invalid_params(format!("admin_lane_keys prefix: {e}")))?;
    let mut keys = store
        .list(&prefix)
        .await
        .map_err(|e| JsonRpcError::internal(format!("admin_lane_keys: {e}")))?;
    keys.sort_by(|x, y| x.path().cmp(y.path()));
    let limit = if a.limit == 0 { 100 } else { a.limit };
    let mut out = Vec::new();
    for k in keys.into_iter().take(limit) {
        let size = store
            .size(&k)
            .await
            .map_err(|e| JsonRpcError::internal(format!("admin_lane_keys size: {e}")))?;
        out.push(json!({ "key": k.path(), "size_bytes": size }));
    }
    Ok(json!({ "keys": out }))
}

#[derive(Deserialize)]
struct AdminLaneBlobArgs {
    #[serde(default)]
    lane: String,
    key: String,
}

async fn tool_admin_lane_blob(indexer: &Indexer, args: Value) -> Result<Value, JsonRpcError> {
    let a: AdminLaneBlobArgs = serde_json::from_value(args)
        .map_err(|e| JsonRpcError::invalid_params(format!("admin_lane_blob: {e}")))?;
    lane_name_ok(&a.lane)?;
    let store = indexer.lane_store();
    let key = Key::new(indexer.tenant(), a.key.clone())
        .map_err(|e| JsonRpcError::invalid_params(format!("admin_lane_blob key: {e}")))?;
    let size = store.size(&key).await.map_err(map_lane_err)?;
    if size > LANE_BLOB_MAX_BYTES {
        return Err(JsonRpcError::invalid_params(format!(
            "blob is {size} bytes, over the {LANE_BLOB_MAX_BYTES}-byte admin cap"
        )));
    }
    let bytes = store.read(&key).await.map_err(map_lane_err)?;
    to_value(AdminLaneBlobResponse {
        bytes_base64: B64.encode(&bytes),
        content_type: lane_content_type(&a.key).to_owned(),
    })
}

fn map_lane_err(e: StoreError) -> JsonRpcError {
    match e {
        StoreError::NotFound(_) => JsonRpcError::invalid_params("lane key not found".to_owned()),
        other => JsonRpcError::internal(format!("lane: {other}")),
    }
}

#[derive(Deserialize)]
struct AdminDeleteChatHistoryArgs {
    #[serde(default)]
    chat_group_id: Option<String>,
    #[serde(default)]
    before_ts: Option<String>,
    #[serde(default)]
    author: Option<String>,
}

async fn tool_admin_delete_chat_history(
    indexer: &Indexer,
    args: Value,
) -> Result<Value, JsonRpcError> {
    let a: AdminDeleteChatHistoryArgs = serde_json::from_value(args)
        .map_err(|e| JsonRpcError::invalid_params(format!("admin_delete_chat_history: {e}")))?;
    let deleted = indexer
        .delete_chat_history(
            a.chat_group_id.as_deref(),
            a.before_ts.as_deref(),
            a.author.as_deref(),
        )
        .await
        .map_err(|e| JsonRpcError::internal(format!("admin_delete_chat_history: {e}")))?;
    Ok(json!({ "deleted": deleted }))
}

// --- admin tenant CRUD + long-ops (admin-role gated) -----------
//
// These port the gRPC `EscurelAdmin` business logic verbatim; only
// the transport wrapper changes. The role gate is applied by the
// dispatcher (`require_admin`) before these run. gRPC error codes
// (not_found / invalid_argument / failed_precondition) map onto the
// JSON-RPC `internal` / `invalid_params` envelope with a clear
// message.

/// `state.tenant_store` or a failed-precondition error mirroring the
/// gRPC `tenant_store()` accessor.
fn tenant_store(state: &AppState) -> Result<&Arc<dyn TenantStore>, JsonRpcError> {
    state
        .tenant_store
        .as_ref()
        .ok_or_else(|| JsonRpcError::internal("server has no tenant_store wired"))
}

/// `state.indexer` or a failed-precondition error.
fn admin_indexer(state: &AppState) -> Result<&Arc<Indexer>, JsonRpcError> {
    state
        .indexer
        .as_ref()
        .ok_or_else(|| JsonRpcError::internal("server has no indexer wired"))
}

/// Map an `AdminError` onto the JSON-RPC envelope, mirroring the
/// gRPC status mapping: invalid id → invalid_params; everything else
/// (already-exists, I/O, duckdb) → internal.
fn map_admin_err(e: escurel_admin::AdminError) -> JsonRpcError {
    match e {
        escurel_admin::AdminError::InvalidTenantId(_) => {
            JsonRpcError::invalid_params(e.to_string())
        }
        other => JsonRpcError::internal(other.to_string()),
    }
}

#[derive(Deserialize)]
struct TenantSpecArgs {
    #[serde(default)]
    tenant_id: String,
    #[serde(default)]
    display_name: String,
}

#[derive(Deserialize)]
struct TenantIdArgs {
    #[serde(default)]
    tenant_id: String,
}

async fn tool_tenant_create(state: &AppState, args: Value) -> Result<Value, JsonRpcError> {
    let a: TenantSpecArgs = serde_json::from_value(args)
        .map_err(|e| JsonRpcError::invalid_params(format!("tenant_create: {e}")))?;
    let store = tenant_store(state)?.clone();
    let spec = AdminTenantSpec {
        tenant_id: a.tenant_id,
        display_name: a.display_name,
    };
    store.create(&spec).await.map_err(map_admin_err)?;
    to_value(TenantCreateResponse {
        spec: Some(TypesTenantSpec {
            tenant_id: spec.tenant_id,
            display_name: spec.display_name,
        }),
    })
}

async fn tool_tenant_list(state: &AppState) -> Result<Value, JsonRpcError> {
    let store = tenant_store(state)?.clone();
    let specs = store.list().await.map_err(map_admin_err)?;
    to_value(TenantListResponse {
        tenants: specs
            .into_iter()
            .map(|s| TypesTenantSpec {
                tenant_id: s.tenant_id,
                display_name: s.display_name,
            })
            .collect(),
    })
}

async fn tool_tenant_get(state: &AppState, args: Value) -> Result<Value, JsonRpcError> {
    let a: TenantIdArgs = serde_json::from_value(args)
        .map_err(|e| JsonRpcError::invalid_params(format!("tenant_get: {e}")))?;
    let store = tenant_store(state)?.clone();
    match store.get(&a.tenant_id).await.map_err(map_admin_err)? {
        None => Err(JsonRpcError::invalid_params(format!(
            "tenant `{}` not found",
            a.tenant_id
        ))),
        Some(spec) => to_value(TenantGetResponse {
            spec: Some(TypesTenantSpec {
                tenant_id: spec.tenant_id,
                display_name: spec.display_name,
            }),
        }),
    }
}

async fn tool_tenant_update(state: &AppState, args: Value) -> Result<Value, JsonRpcError> {
    let a: TenantSpecArgs = serde_json::from_value(args)
        .map_err(|e| JsonRpcError::invalid_params(format!("tenant_update: {e}")))?;
    let store = tenant_store(state)?.clone();
    let spec = AdminTenantSpec {
        tenant_id: a.tenant_id,
        display_name: a.display_name,
    };
    store.update(&spec).await.map_err(map_admin_err)?;
    to_value(TenantUpdateResponse {
        spec: Some(TypesTenantSpec {
            tenant_id: spec.tenant_id,
            display_name: spec.display_name,
        }),
    })
}

async fn tool_tenant_delete(state: &AppState, args: Value) -> Result<Value, JsonRpcError> {
    let a: TenantIdArgs = serde_json::from_value(args)
        .map_err(|e| JsonRpcError::invalid_params(format!("tenant_delete: {e}")))?;
    let store = tenant_store(state)?.clone();
    let deleted = store.delete(&a.tenant_id).await.map_err(map_admin_err)?;
    to_value(TenantDeleteResponse { deleted })
}

async fn tool_tenant_export(state: &AppState, args: Value) -> Result<Value, JsonRpcError> {
    let a: TenantIdArgs = serde_json::from_value(args)
        .map_err(|e| JsonRpcError::invalid_params(format!("tenant_export: {e}")))?;
    let store = tenant_store(state)?.clone();
    // Validate before constructing on-disk paths — `tenant_dir` is
    // filesystem-direct and would happily resolve `../other`.
    validate_tenant_id(&a.tenant_id).map_err(|e| JsonRpcError::invalid_params(e.to_string()))?;
    let tenant_dir = store
        .tenant_dir(&a.tenant_id)
        .ok_or_else(|| JsonRpcError::internal("tenant store has no on-disk path"))?;
    // Spec (storage.md): only canonical markdown is exported.
    let markdown_dir = tenant_dir.join("markdown");
    if !tokio::fs::try_exists(&markdown_dir).await.unwrap_or(false) {
        return Err(JsonRpcError::invalid_params(format!(
            "tenant `{}` not found",
            a.tenant_id
        )));
    }
    // Build the whole tarball in memory on a blocking thread (file
    // I/O + zlib). The MCP transport is one-shot, so we accumulate
    // every chunk rather than streaming.
    const CHUNK: usize = 64 * 1024;
    let bytes = tokio::task::spawn_blocking(move || -> std::io::Result<Vec<u8>> {
        let mut out: Vec<u8> = Vec::new();
        tar_gz_into_chunks(&markdown_dir, CHUNK, |chunk| {
            out.extend_from_slice(&chunk);
            Ok(())
        })?;
        Ok(out)
    })
    .await
    .map_err(|e| JsonRpcError::internal(format!("tenant_export join error: {e}")))?
    .map_err(|e| JsonRpcError::internal(format!("tenant_export: {e}")))?;
    let len = bytes.len() as u64;
    Ok(json!({ "tarball_b64": B64.encode(&bytes), "bytes": len }))
}

#[derive(Deserialize)]
struct TenantImportArgs {
    #[serde(default)]
    tenant_id: String,
    #[serde(default)]
    tarball_b64: String,
}

async fn tool_tenant_import(state: &AppState, args: Value) -> Result<Value, JsonRpcError> {
    let a: TenantImportArgs = serde_json::from_value(args)
        .map_err(|e| JsonRpcError::invalid_params(format!("tenant_import: {e}")))?;
    let store = tenant_store(state)?.clone();
    validate_tenant_id(&a.tenant_id).map_err(|e| JsonRpcError::invalid_params(e.to_string()))?;
    // The target tenant must exist before import (mirrors gRPC).
    if store
        .get(&a.tenant_id)
        .await
        .map_err(map_admin_err)?
        .is_none()
    {
        return Err(JsonRpcError::invalid_params(format!(
            "tenant `{}` not found",
            a.tenant_id
        )));
    }
    let tenant_dir = store
        .tenant_dir(&a.tenant_id)
        .ok_or_else(|| JsonRpcError::internal("tenant store has no on-disk path"))?;
    let markdown_dir = tenant_dir.join("markdown");
    let buf = B64
        .decode(a.tarball_b64.as_bytes())
        .map_err(|e| JsonRpcError::invalid_params(format!("tarball_b64 is not base64: {e}")))?;
    let bytes_imported = buf.len() as u64;
    tokio::task::spawn_blocking(move || untar_gz_into(&buf, &markdown_dir))
        .await
        .map_err(|e| JsonRpcError::internal(format!("tenant_import join error: {e}")))?
        .map_err(|e| JsonRpcError::internal(format!("tenant_import: {e}")))?;
    to_value(TenantImportResponse { bytes_imported })
}

#[derive(Deserialize)]
struct AttachExternalArgs {
    #[serde(default)]
    alias: String,
    #[serde(default)]
    source_url: String,
}

async fn tool_attach_external(state: &AppState, args: Value) -> Result<Value, JsonRpcError> {
    let a: AttachExternalArgs = serde_json::from_value(args)
        .map_err(|e| JsonRpcError::invalid_params(format!("attach_external: {e}")))?;
    let indexer = admin_indexer(state)?.clone();
    // Reject an unsafe source before it reaches the ATTACH SQL.
    // DuckDB has no parameter binding for ATTACH, so this is the
    // injection boundary (the indexer re-checks defensively).
    if !is_safe_attach_source(&a.source_url) {
        return Err(JsonRpcError::invalid_params(
            "source_url contains an unsafe character (quote, backslash, semicolon, \
             or control char) or is empty"
                .to_owned(),
        ));
    }
    indexer
        .attach_external(&a.alias, &a.source_url)
        .await
        .map_err(|e| JsonRpcError::internal(format!("attach_external: {e}")))?;
    to_value(AttachExternalResponse { source_id: a.alias })
}

async fn tool_embedding_reload(state: &AppState) -> Result<Value, JsonRpcError> {
    // The reloadable seam + the rebuild factory are wired together:
    // without both there is nothing to reload.
    let (reload, factory) = match (&state.embedder_reload, &state.embedder_factory) {
        (Some(r), Some(f)) => (r, f),
        _ => {
            return Err(JsonRpcError::internal("no reloadable embedder configured"));
        }
    };
    let (embedder, model_revision) = factory()
        .await
        .map_err(|e| JsonRpcError::internal(format!("embedding_reload: model load failed: {e}")))?;
    reload.reload(embedder);
    to_value(EmbeddingReloadResponse { model_revision })
}

async fn tool_rebuild(state: &AppState, args: Value) -> Result<Value, JsonRpcError> {
    let a: TenantIdArgs = serde_json::from_value(args)
        .map_err(|e| JsonRpcError::invalid_params(format!("rebuild: {e}")))?;
    if !a.tenant_id.is_empty() {
        validate_tenant_id(&a.tenant_id)
            .map_err(|e| JsonRpcError::invalid_params(e.to_string()))?;
    }
    let indexer = admin_indexer(state)?.clone();
    // Capture the last (done, total) the progress callback reports.
    // The MCP transport returns the terminal counts rather than a
    // progress stream.
    let last = Arc::new(std::sync::Mutex::new((0u64, 0u64)));
    let sink = Arc::clone(&last);
    indexer
        .rebuild_with_progress(move |p| {
            if let Ok(mut g) = sink.lock() {
                *g = (p.done, p.total);
            }
        })
        .await
        .map_err(|e| JsonRpcError::internal(format!("rebuild: {e}")))?;
    let (done, total) = *last.lock().expect("rebuild progress lock");
    to_value(RebuildProgress {
        done,
        total,
        current_page: String::new(),
    })
}

async fn tool_compact_lanes(state: &AppState, args: Value) -> Result<Value, JsonRpcError> {
    let a: TenantIdArgs = serde_json::from_value(args)
        .map_err(|e| JsonRpcError::invalid_params(format!("compact_lanes: {e}")))?;
    validate_tenant_id(&a.tenant_id).map_err(|e| JsonRpcError::invalid_params(e.to_string()))?;
    let backend = state
        .crdt_backend
        .as_ref()
        .ok_or_else(|| JsonRpcError::internal("server has no crdt_backend wired"))?
        .clone();
    let pages = backend
        .pages_with_snapshots()
        .await
        .map_err(|e| JsonRpcError::internal(format!("compact_lanes: list pages: {e}")))?;
    let mut ops_compacted = 0u64;
    let mut bytes_reclaimed = 0u64;
    for page_id in pages {
        let (ops, bytes) = backend
            .compact_subsumed_ops(&page_id)
            .await
            .map_err(|e| JsonRpcError::internal(format!("compact_lanes: page `{page_id}`: {e}")))?;
        ops_compacted += ops;
        bytes_reclaimed += bytes;
    }
    to_value(CompactProgress {
        ops_compacted,
        bytes_reclaimed,
    })
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
                "Enumerate instances of a skill, optionally filtered by a frontmatter field.",
                json!({
                    "type": "object",
                    "required": ["skill_id"],
                    "properties": {
                        "skill_id": { "type": "string" },
                        "order_by": { "type": "string", "enum": ["at asc", "at desc"] },
                        "limit": { "type": "integer", "minimum": 1, "maximum": 10000 },
                        "frontmatter_key": { "type": "string", "description": "Frontmatter field to filter on (with frontmatter_value)." },
                        "frontmatter_value": { "type": "string", "description": "Required value of frontmatter_key." },
                        "as_of": { "type": "string", "description": "RFC 3339 time-travel cut; instances born after it are excluded (untimed always remain)." },
                        "scenario": { "type": "string", "description": "What-if overlay; absent = base only, else base ∪ overlay (overlay wins per slug)." }
                    }
                }),
            ),
            tool_entry(
                "resolve",
                "Parse a [[wikilink]] and look up its target page.",
                json!({
                    "type": "object",
                    "required": ["wikilink"],
                    "properties": {
                        "wikilink": { "type": "string" },
                        "scenario": { "type": "string", "description": "What-if overlay to resolve against; absent = base only." }
                    }
                }),
            ),
            tool_entry(
                "expand",
                "Fetch a page's frontmatter + body + outbound wikilinks.",
                json!({
                    "type": "object",
                    "required": ["page_id"],
                    "properties": {
                        "page_id": { "type": "string" },
                        "as_of": { "type": "string", "description": "RFC 3339 time-travel cut; the page is null if born after it." },
                        "scenario": { "type": "string", "description": "What-if overlay to read against; absent = base only." }
                    }
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
                        "link_skill": { "type": "string" },
                        "as_of": { "type": "string", "description": "RFC 3339 time-travel cut; edges from sources born after it are hidden." },
                        "scenario": { "type": "string", "description": "What-if overlay; edges filtered by their source page's scenario." }
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
                        "granularity": { "type": "string", "enum": ["block", "page"], "description": "Result granularity; `page` collapses block hits to one per page. Default `block`." },
                        "page_type": { "type": "string", "enum": ["skill", "instance", "any"] },
                        "skill": { "type": "string" },
                        "filter": { "type": "object", "description": "Frontmatter post-filter; clauses are ANDed, e.g. {\"tier\": \"gold\", \"at\": {\">=\": \"2026-04-01\"}}." },
                        "as_of": { "type": "string", "description": "RFC 3339 time-travel cut; blocks born after it are excluded." },
                        "scenario": { "type": "string", "description": "What-if overlay; base-only when absent." }
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
                "validate",
                "Dry-run the indexer's checks on a draft; returns the same issue list \
                 as update_page but commits nothing.",
                json!({
                    "type": "object",
                    "required": ["content"],
                    "properties": {
                        "content": { "type": "string" },
                        "as_page_id": { "type": "string" }
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
                "append_message",
                "Append a message to a chat-group's conversation history. \
                 `chat_group_id` is opaque to escurel; consumers own the \
                 identifier scheme. `embed` defaults to true; set false to \
                 skip the embedding cost for high-volume sources.",
                json!({
                    "type": "object",
                    "required": ["chat_group_id", "role", "content"],
                    "properties": {
                        "chat_group_id": { "type": "string" },
                        "role": {
                            "type": "string",
                            "enum": ["user", "assistant", "system", "tool"]
                        },
                        "content": { "type": "string" },
                        "author": { "type": "string" },
                        "ts": {
                            "type": "string",
                            "description": "RFC-3339 UTC; server stamps CURRENT_TIMESTAMP when absent"
                        },
                        "metadata": { "type": "object" },
                        "msg_id": {
                            "type": "string",
                            "description": "Caller-supplied id; server generates a ULID when absent"
                        },
                        "embed": { "type": "boolean", "default": true }
                    }
                }),
            ),
            tool_entry(
                "list_messages",
                "Read back a chat-group's conversation history time-ordered. \
                 `since` is inclusive, `until` is exclusive. `direction` \
                 defaults to `desc` (most recent first). Use `next_cursor` \
                 to page.",
                json!({
                    "type": "object",
                    "required": ["chat_group_id"],
                    "properties": {
                        "chat_group_id": { "type": "string" },
                        "since": { "type": "string" },
                        "until": { "type": "string" },
                        "limit": {
                            "type": "integer",
                            "minimum": 1,
                            "maximum": 1000,
                            "default": 100
                        },
                        "cursor": { "type": "string" },
                        "direction": {
                            "type": "string",
                            "enum": ["asc", "desc"],
                            "default": "desc"
                        }
                    }
                }),
            ),
            tool_entry(
                "capture_event",
                "Append an event to the global inbox (M7). `label_skill` links \
                 to the skill that knows how to process this event type; \
                 `instance_page_id` may pre-flag a candidate instance but the \
                 event stays in the inbox until `assign_event`. Returns the \
                 stored event with its id + timestamp.",
                json!({
                    "type": "object",
                    "properties": {
                        "event_id": { "type": "string", "description": "Caller-supplied id; server generates a ULID when absent." },
                        "at": { "type": "string", "description": "RFC 3339 event time." },
                        "source": { "type": "string", "description": "Ingest source, e.g. gmail/meet/drive." },
                        "mime": { "type": "string", "description": "Content type, e.g. message/rfc822." },
                        "label_skill": { "type": "string", "description": "Skill id: how to process this event type." },
                        "instance_page_id": { "type": "string", "description": "Candidate instance (label hint); still inbox until assigned." },
                        "title": { "type": "string" },
                        "body": { "type": "string" },
                        "provenance": { "type": "object" }
                    }
                }),
            ),
            tool_entry(
                "list_inbox",
                "List unprocessed events (the inbox), newest first.",
                json!({
                    "type": "object",
                    "properties": {
                        "limit": { "type": "integer", "minimum": 1, "maximum": 10000 }
                    }
                }),
            ),
            tool_entry(
                "list_events",
                "List an instance's processed event history (the event sequence \
                 whose projection is its state), oldest first.",
                json!({
                    "type": "object",
                    "required": ["instance_page_id"],
                    "properties": {
                        "instance_page_id": { "type": "string" },
                        "limit": { "type": "integer", "minimum": 1, "maximum": 10000 }
                    }
                }),
            ),
            tool_entry(
                "list_snapshots",
                "List the taken_at timestamps of an instance's CRDT snapshot \
                 history, oldest first — the discrete state-over-time points \
                 expand(as_of=T) can replay.",
                json!({
                    "type": "object",
                    "required": ["page_id"],
                    "properties": {
                        "page_id": { "type": "string" }
                    }
                }),
            ),
            tool_entry(
                "assign_event",
                "Assign an inbox event to an instance and mark it processed — the \
                 (external) agent folding the event into the instance.",
                json!({
                    "type": "object",
                    "required": ["event_id", "instance_page_id"],
                    "properties": {
                        "event_id": { "type": "string" },
                        "instance_page_id": { "type": "string" }
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
            // Admin-gated ops tools. Visible in tools/list, but the
            // dispatcher rejects non-admin callers (see require_admin).
            tool_entry(
                "admin_quota",
                "Admin: per-tenant quota snapshot (remaining query/write/embed \
                 budget + concurrent sessions in use).",
                json!({ "type": "object", "properties": {} }),
            ),
            tool_entry(
                "admin_audit",
                "Admin: drift between canonical markdown and the DuckDB index \
                 (markdown_not_in_duckdb / indexed_but_no_markdown).",
                json!({ "type": "object", "properties": {} }),
            ),
            tool_entry(
                "admin_index_query",
                "Admin: read up to `limit` rows from an allow-listed index table \
                 (pages, blocks, links, frontmatter_index, crdt_ops, crdt_snapshots, \
                 chat_messages). Not arbitrary SQL.",
                json!({
                    "type": "object",
                    "required": ["table"],
                    "properties": {
                        "table": {
                            "type": "string",
                            "enum": ["pages", "blocks", "links", "frontmatter_index",
                                     "crdt_ops", "crdt_snapshots", "chat_messages"]
                        },
                        "limit": { "type": "integer", "minimum": 1, "maximum": 1000, "default": 100 }
                    }
                }),
            ),
            tool_entry(
                "admin_delete_chat_history",
                "Admin: purge chat history. GDPR erasure of a whole group \
                 (chat_group_id set) or a single member across groups \
                 (author set), retention prune (before_ts set); filters \
                 compose with AND. MCP twin of the gRPC \
                 EscurelAdmin.DeleteChatHistory.",
                json!({
                    "type": "object",
                    "properties": {
                        "chat_group_id": { "type": "string" },
                        "before_ts": { "type": "string" },
                        "author": { "type": "string" }
                    }
                }),
            ),
            tool_entry(
                "admin_list_lanes",
                "Admin: enumerate the configured LaneStores (name, backend, \
                 tenants present). MCP twin of EscurelAdmin.AdminListLanes.",
                json!({ "type": "object", "properties": {} }),
            ),
            tool_entry(
                "admin_lane_keys",
                "Admin: list keys under a prefix in a lane, with byte sizes. \
                 MCP twin of EscurelAdmin.AdminLaneKeys.",
                json!({
                    "type": "object",
                    "properties": {
                        "lane": { "type": "string", "description": "Lane name; empty = the default `markdown`." },
                        "prefix": { "type": "string", "description": "Tenant-relative key prefix." },
                        "limit": { "type": "integer", "minimum": 0, "description": "0 → server default (100)." }
                    }
                }),
            ),
            tool_entry(
                "admin_lane_blob",
                "Admin: fetch one blob (base64) from a lane, subject to a \
                 1 MiB cap. MCP twin of EscurelAdmin.AdminLaneBlob.",
                json!({
                    "type": "object",
                    "required": ["key"],
                    "properties": {
                        "lane": { "type": "string" },
                        "key": { "type": "string" }
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
