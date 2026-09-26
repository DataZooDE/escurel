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
use escurel_auth::Role;
use escurel_crdt::{
    CrdtBackend, Op, Snapshot, Version, hydrate_content, snapshot_bytes_from_markdown,
    three_way_merge,
};
use escurel_index::snapshot::{gc_lake_snapshots, publish_lake};
use escurel_index::{
    AclCaller, AppendChatMessage, Capabilities, ChatMessage, Direction, EventInfo, Granularity,
    GraphDir, Indexer, IndexerError, IndexerHandle, Issue, ListChatMessages, NewEvent, OrderDir,
    Severity, Visibility, derive_attach_alias, is_safe_attach_source,
};
use escurel_md::PageType;
use escurel_quota::{Dimension, QuotaError, QuotaManager};
use escurel_storage::{Key, StoreError};
use escurel_types::{
    AdminLaneBlobResponse, AttachExternalResponse, CompactProgress, EmbeddingReloadResponse,
    ListSkillsResponse, PublishSnapshotResponse, QuotaGetResponse, RebuildProgress,
    Skill as TypesSkill, SkillAcl as TypesSkillAcl, SkillBackend as TypesSkillBackend,
    SkillCapabilities as TypesSkillCapabilities, SkillField as TypesSkillField,
    SkillParam as TypesSkillParam, TenantCreateResponse, TenantDeleteResponse, TenantGetResponse,
    TenantImportResponse, TenantListResponse, TenantSpec as TypesTenantSpec, TenantUpdateResponse,
    WebhookDeliveriesResponse, WebhookDelivery,
};
use serde::Deserialize;
use serde_json::{Value, json};
use tracing::Instrument;

mod backend_view;
mod ingest;
mod schema;
mod tools_admin;
mod tools_branches;
mod tools_control;
mod tools_drafts;
mod tools_lineage;
mod tools_mint;
pub(crate) use tools_mint::sweep_expired_minted_runs;
mod tools_progress;
mod tools_tool_calls;
pub use tools_progress::DEFAULT_RUN_PROGRESS_KEEP;
mod tools_read;
mod tools_write;
pub(crate) use ingest::{blob_get, ingest, ingest_upload};
pub(crate) use schema::openapi_document;
use schema::page_type_str;
use tools_admin::*;
use tools_branches::*;
use tools_drafts::*;
use tools_lineage::*;
use tools_progress::*;
use tools_read::*;
pub(crate) use tools_write::event_to_json;

/// The `search` tool for the WS live-search subscription (#355): same
/// ACL-fused hybrid search, errors flattened to their message (the
/// JSON-RPC envelope is an HTTP concern).
pub(crate) async fn ws_search(
    indexer: &escurel_index::Indexer,
    caller: escurel_index::acl::AclCaller<'_>,
    args: Value,
) -> Result<Value, String> {
    tools_read::tool_search(indexer, caller, args)
        .await
        .map_err(|e| e.message)
}
pub(crate) use tools_write::stamped_principal;
use tools_write::*;

use crate::server::AppState;
use crate::session::{SessionError, SessionManager};
use crate::tenant_archive::{tar_gz_into_chunks, untar_gz_into};

/// JSON-RPC 2.0 request envelope.
#[derive(Debug, Deserialize)]
pub struct JsonRpcRequest {
    pub jsonrpc: String,
    /// `#[serde(default)]` so a JSON-RPC *notification* (which omits
    /// `id`) still deserializes — `id` becomes `Value::Null`. The
    /// MCP lifecycle drives `notifications/initialized` after the
    /// handshake, and those carry no `id`.
    #[serde(default)]
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

/// MCP entry point: `POST /mcp` — a spec-compliant MCP
/// **Streamable-HTTP** server. Drives the full client lifecycle:
/// `initialize` (handshake → `InitializeResult`), the
/// `notifications/initialized` notification (→ HTTP 202, empty
/// body — a notification gets no JSON-RPC response), `ping`, and
/// the `tools/list` / `tools/call` calls. Each request is a single
/// JSON response (`Content-Type: application/json`); the optional
/// SSE / `GET /mcp` streaming transport is not implemented (the
/// Streamable-HTTP spec permits a JSON-only response).
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
    // The run this call belongs to, when a runner made it. Recorded as its
    // own field rather than only as the `request_id` prefix so a single
    // `run_id=<ulid>` query returns every tool call the run made, in order,
    // alongside the runner's own lines for it. Empty for every other caller.
    let run_id = headers
        .get("x-escurel-run-id")
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .unwrap_or_default()
        .to_owned();
    let tool_name = tool_name_from(&req.method, &req.params).unwrap_or_default();
    // Per-record audit fields per `platform.md §Observability`:
    // `transport` + `trace_id` are known up front; `tenant` + `subject`
    // are filled in (`span.record`) once auth resolves. The JSON
    // formatter hoists all span fields onto every record emitted
    // inside the span, so the `tool.completed` event below carries the
    // full contract set (tenant/tool/transport/subject/trace_id/
    // duration_ms). `trace_id` mirrors the gateway `request_id` when no
    // OTel trace context is active.
    // The run-bound fields (P3-3) are declared empty and recorded once auth
    // says whose call this is: an OpenInference `TOOL` span on the run's
    // own trace, sizes only.
    let span = tracing::info_span!(
        "mcp.request",
        request_id = %request_id,
        trace_id = %request_id,
        run_id = %run_id,
        transport = "mcp_http",
        method = %req.method,
        tool = %tool_name,
        openinference.span.kind = tracing::field::Empty,
        escurel.run_id = tracing::field::Empty,
        escurel.root_event_id = tracing::field::Empty,
        input.size = tracing::field::Empty,
        output.size = tracing::field::Empty,
    );
    // Authenticate BEFORE the span is entered: a run-bound bearer's call is
    // a TOOL span on the run's own trace (P3-3), and an OTel parent can
    // only be set on a span that has not started yet.
    let auth = crate::auth_gate::authenticate(&state, &headers).await;
    if let Ok(Some(ctx)) = &auth
        && let Some(run) = &ctx.run
    {
        span.record("openinference.span.kind", "TOOL");
        span.record("escurel.run_id", run.run_id.as_str());
        if let Some(root) = &run.root_event_id {
            span.record("escurel.root_event_id", root.as_str());
        }
        if let Some(trace) = &run.trace_id {
            // The request id seeds the parent's span id, so a retry lands
            // on the same parent.
            escurel_obs::attach_run_trace(&span, trace, &request_id);
        }
    }
    mcp_inner(state, req, auth).instrument(span).await
}

async fn mcp_inner(
    state: crate::server::AppState,
    mut req: JsonRpcRequest,
    auth: Result<Option<escurel_auth::AuthContext>, axum::response::Response>,
) -> axum::response::Response {
    tracing::info!(msg = "mcp.request.start", "mcp.request.start");

    if req.jsonrpc != "2.0" {
        return error_response(req.id, -32600, "invalid jsonrpc version");
    }

    // Auth gate — only enforced when a verifier is configured; resolved by
    // the outer handler (before the span started) and handed in.
    let auth_ctx = match auth {
        Ok(ctx) => ctx,
        Err(resp) => return resp,
    };

    // Dispatch-level tool ALIASES (API review B1): the verb-first
    // spellings of the noun-first stragglers resolve to their canonical
    // names here, before anything keys on the name — quota, metrics,
    // replica gates and the dispatch match all see the canonical
    // spelling. `tools/list` advertises canonical names only.
    if req.method == "tools/call"
        && let Some(name) = req.params.get("name").and_then(Value::as_str)
        && let Some(canonical) = schema::canonical_tool_name(name)
    {
        req.params["name"] = json!(canonical);
    }

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

    // #247 suspend gate: a suspended tenant rejects non-admin callers so an
    // operator can still `resume` with an admin token. Only bites when a
    // verifier is wired (a concrete non-admin role); dev/on-host mode
    // (`role: None` ⇒ treated as admin elsewhere) is unaffected.
    if state
        .tenant_suspended
        .load(std::sync::atomic::Ordering::Relaxed)
        && matches!(role, Some(Role::Agent))
    {
        let body = json!({
            "jsonrpc": "2.0",
            "id": req.id,
            "error": {
                "code": -32003,
                "message": format!("tenant `{tenant_id}` is suspended"),
                "data": { "code": "tenant_suspended", "retryable": false },
            }
        });
        return (StatusCode::FORBIDDEN, Json(body)).into_response();
    }

    // Auth-derived audit fields for the `tool.completed` record.
    // `subject` is the token `sub` claim; `anonymous` in
    // unauthenticated dev mode.
    let subject = auth_ctx
        .as_ref()
        .map(|c| c.subject.clone())
        .unwrap_or_else(|| "anonymous".to_owned());

    // RBAC token groups, projected from the JWT `groups_claim`
    // (escurel-auth). The configured `admin_role_value` (e.g.
    // `escurel:admin`) is stripped here so it can never act as an ordinary
    // group name — admin authority comes only from the verified role, never
    // a header grant. Reserved names (public/owner/admin) are stripped
    // again inside escurel-index as defence in depth.
    let token_groups = crate::auth_gate::rbac_groups(&state, auth_ctx.as_ref());

    // Who the subject is acting FOR (#510), from the verified `act.sub`: a
    // per-run agent token names the runner that delegated to it. Audit
    // lineage only — never read for an authorization decision.
    let actor = auth_ctx.as_ref().and_then(|c| c.actor.clone());
    // The run this token was minted for (workbench backend P1): a per-run
    // agent bearer names its run; the gateway stamps that lineage onto what
    // the run writes. Never read for an authorization decision.
    let run = auth_ctx.as_ref().and_then(|c| c.run.clone());
    let agent_skill = auth_ctx.as_ref().and_then(|c| c.agent_skill.clone());

    // JSON-RPC notifications (no `id`, method `notifications/*`) get
    // NO response envelope — the MCP Streamable-HTTP spec says the
    // server acknowledges with HTTP 202 Accepted and an empty body.
    // The client posts `notifications/initialized` right after the
    // `initialize` handshake; we 202 any `notifications/*` and never
    // error on an unknown one.
    if req.method.starts_with("notifications/") {
        return StatusCode::ACCEPTED.into_response();
    }

    let result = match req.method.as_str() {
        "initialize" => Ok(initialize_result(&req.params)),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(schema::tools_list_payload_for(role)),
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
            // For the run's tool-call record (P3-1): the argument size, in
            // bytes — never the arguments.
            let request_bytes = req
                .params
                .get("arguments")
                .map(|a| a.to_string().len())
                .unwrap_or(0);
            // MCP-shape the SUCCESS payload into a `CallToolResult`
            // (`content` + `structuredContent` + `isError:false`) so real
            // MCP clients (Claude Code) can READ the tool output. Tool
            // ERRORS keep the JSON-RPC error envelope (the `Err` arm of
            // the outer `match result`) — only the success value is
            // wrapped. `initialize` / `ping` / `tools/list` are NOT
            // CallToolResults and are returned raw above.
            let r = dispatch_tools_call(
                &state,
                &tenant_id,
                role,
                &subject,
                &token_groups,
                actor.as_deref(),
                run.as_ref(),
                agent_skill.as_deref(),
                req.params,
            )
            .await;
            let rejected = matches!(&r, Ok(payload) if is_rejected_payload(&tool, payload));
            let r = r.map(|payload| wrap_tool_result(payload, rejected));
            let status = if r.is_err() {
                "error"
            } else if rejected {
                "rejected"
            } else {
                "ok"
            };
            // The REASON a tool failed, for the audit record below.
            //
            // Until 2026-08-30 a failed tool logged `status: "error"` and
            // nothing else, so the only trace of `query_instance` failing
            // was an LLM three services away paraphrasing it as "the
            // upstream systems are returning connectivity errors". The
            // actual cause (a `[[query::*]]` page that resolves to
            // nothing) never appeared in any log line on any host.
            //
            // `message` is already the operator-facing string every
            // constructor here builds, and it is already sent to the
            // caller on the wire — recording it locally reveals nothing
            // the client was not told. `data.code` rides along when the
            // error carries one, because it is the stable thing to alert
            // on; `message` is one wording edit from breaking a matcher.
            let error_detail = r.as_ref().err().map(|e| {
                match e
                    .data
                    .as_ref()
                    .and_then(|d| d.get("code"))
                    .and_then(Value::as_str)
                {
                    Some(code) => format!("{code}: {}", e.message),
                    None => e.message.clone(),
                }
            });
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
                error_detail = error_detail.as_deref().unwrap_or(""),
                msg = "tool.completed",
                "tool.completed"
            );
            // The span's sizes (P3-3), for a run's call only.
            if run.is_some() {
                let span = tracing::Span::current();
                span.record("input.size", request_bytes as u64);
                span.record(
                    "output.size",
                    match &r {
                        Ok(v) => v.to_string().len() as u64,
                        Err(e) => e.message.len() as u64,
                    },
                );
            }
            // A run's own record of what it called (workbench backend
            // P3-1): one row per call made with a run-bound bearer — tool,
            // outcome, duration, sizes. Best-effort: the call's result is
            // the call's result, whether or not the row lands.
            if let (Some(run), Some(indexer)) = (
                run.as_ref(),
                state.indexer.as_ref().map(IndexerHandle::current),
            ) {
                let error_code = r.as_ref().err().and_then(|e| {
                    e.data
                        .as_ref()
                        .and_then(|d| d.get("code"))
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                });
                let response_bytes = match &r {
                    Ok(v) => v.to_string().len(),
                    Err(e) => e.message.len(),
                };
                if let Err(e) = indexer
                    .record_tool_call(escurel_index::NewToolCall {
                        run_id: run.run_id.clone(),
                        root_event_id: run.root_event_id.clone(),
                        tool: tool.clone(),
                        // The same three-valued status the metrics and the
                        // audit line carry: a write the gateway REJECTED
                        // (`ok: false`) is neither ok nor a protocol error.
                        status: status.to_owned(),
                        error_code,
                        duration_ms,
                        request_bytes: request_bytes as u64,
                        response_bytes: response_bytes as u64,
                        subject: subject.clone(),
                    })
                    .await
                {
                    tracing::warn!(error = %e, run_id = %run.run_id, tool = %tool, "run_tool_calls: row not recorded");
                }
            }
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

/// `POST /ingest` — the document-ingestion webhook (REQ-DOC-07, HLD §6.2).
///
/// An external uploader deposits the original into the inbox (content-
/// addressed) and then POSTs `{ blob_id, content_type }`. This handler:
/// authenticates + rate-limits per tenant (REQ-NF-07); resolves the content
/// type to a handling document skill via its `accepts:` list (REQ-DOC-06);
/// records an immutable **ingest Event** (the auditable arrival log) whether
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
            "data": { "code": "quota_exhausted", "retryable": true, "dimension": dim, "retry_after_ms": retry }
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
/// Build the MCP `initialize` result. We echo the client's
/// requested `protocolVersion` when it is a non-empty string (maximises
/// compatibility — Claude Code negotiates e.g. `"2025-06-18"`), and
/// fall back to the latest version we speak otherwise.
fn initialize_result(params: &Value) -> Value {
    const DEFAULT_PROTOCOL_VERSION: &str = "2025-06-18";
    let protocol_version = params
        .get("protocolVersion")
        .and_then(Value::as_str)
        .filter(|v| !v.is_empty())
        .unwrap_or(DEFAULT_PROTOCOL_VERSION);
    json!({
        "protocolVersion": protocol_version,
        "capabilities": { "tools": { "listChanged": false } },
        "serverInfo": {
            "name": "escurel",
            "version": env!("CARGO_PKG_VERSION"),
        },
    })
}

/// Maps a JSON-RPC method to the quota dimension it debits, or `None`
/// for methods that consume no tenant rate budget. The lifecycle
/// methods (`initialize`, `ping`, `notifications/*`) and `tools/list`
/// all fall through the `tools/call` guard below and so debit nothing.
fn dimension_for(method: &str, params: &Value) -> Option<Dimension> {
    if method != "tools/call" {
        return None;
    }
    let name = params.get("name").and_then(Value::as_str)?;
    // Admin / operator tools are role-gated, not part of the tenant's
    // *agent* rate budget — they must not debit the query/write
    // buckets (the old gRPC admin surface carried no quota
    // middleware). Otherwise an operator's own `admin_quota` snapshot
    // would read back one-less-than-full. Keyed on the `scope` label
    // (the same declaration `tools/list` filters by and the registry
    // ratchet pins to `require_admin`) — the previous hand-kept
    // prefix + `matches!` list silently forgot every unprefixed admin
    // tool, which then ate the tenant's agent budget (API review).
    if schema::admin_scope_tools().contains(name) {
        return None;
    }
    Some(match name {
        // `apply_op` is a write; `open_session` debits a session
        // slot (semaphore, not a token bucket) inside the tool
        // body; `close_session` is a cleanup and does not debit.
        "update_page" | "delete_page" | "move_page" | "purge_page" | "apply_op"
        | "append_message" | "capture_event" | "assign_event" | "create_draft"
        | "promote_draft" | "discard_draft" | "start_operation" => Dimension::Writes,
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
            data: None,
        }
        .with_code("admin_required", false)),
    }
}

/// Serialize an `escurel_types` response struct to a JSON-RPC result
/// value. The escurel-types structs are the wire contract; a
/// serialization failure here is a server bug, surfaced as internal.
/// Deserialize a tool's arguments, reporting a parse failure as
/// `invalid_params` tagged with the tool name.
///
/// Every tool handler opened with the same two-line incantation; naming it
/// once means a handler's first line is about the tool rather than about
/// serde. See `docs/notes/complexity-reduction-plan.md` R4.
fn parse_args<T: serde::de::DeserializeOwned>(args: Value, tool: &str) -> Result<T, JsonRpcError> {
    serde_json::from_value(args).map_err(|e| JsonRpcError::invalid_params(format!("{tool}: {e}")))
}

fn to_value<T: serde::Serialize>(resp: T) -> Result<Value, JsonRpcError> {
    serde_json::to_value(resp)
        .map_err(|e| JsonRpcError::internal(format!("serialize response: {e}")))
}

/// MCP-shape a `tools/call` success payload into the spec's
/// [`CallToolResult`](https://modelcontextprotocol.io/specification)
/// envelope:
///
/// ```jsonc
/// {
///   "content": [ { "type": "text", "text": "<payload as JSON string>" } ],
///   "structuredContent": <the raw payload object>,
///   "isError": <rejected>
/// }
/// ```
///
/// `content[0].text` is the payload serialised to a JSON string — that
/// is what a text-only MCP client (Claude Code) reads. `structuredContent`
/// carries the raw payload object for programmatic clients (escurel-client
/// decodes this). `isError` is true when the payload is a refusal (see
/// [`is_rejected_payload`]). Applied to the SUCCESS value of `tools/call`
/// ONLY; tool errors keep the JSON-RPC error envelope, and `initialize` /
/// `ping` / `tools/list` are returned raw (they are not `CallToolResult`s).
fn wrap_tool_result(payload: Value, rejected: bool) -> Value {
    let text = serde_json::to_string(&payload).unwrap_or_else(|_| payload.to_string());
    json!({
        "content": [ { "type": "text", "text": text } ],
        "structuredContent": payload,
        "isError": rejected,
    })
}

/// Whether a tool's `Ok` payload is a REFUSAL — the call ran, nothing was
/// applied, and only `ok:false` says so (#373). These get
/// `CallToolResult.isError: true` (MCP's signal for "the call did not do
/// what was asked") and `status: "rejected"` in the `tool.completed`
/// record, while the payload contract stays byte-identical.
///
/// Two `ok:false` shapes are NOT refusals and stay `isError:false`:
/// - `validate`, whose entire job is reporting issues — an issue list IS
///   that tool succeeding;
/// - any `dry_run:true` payload (`rebase_pack`), where `ok` answers
///   "would a real run apply?" — a plan, not a failed action.
fn is_rejected_payload(tool: &str, payload: &Value) -> bool {
    payload.get("ok") == Some(&Value::Bool(false))
        && tool != "validate"
        && payload.get("dry_run") != Some(&Value::Bool(true))
}

/// Tool surface a ducklake reader must reject outright when the
/// deployment has no relevant shared backend attached. This static list
/// is now EMPTY of CRDT/session tools — DuckLake PR 10 (Phase B) moved
/// `open_session` / `apply_op` / `close_session` / `list_snapshots` to
/// the dynamic [`CRDT_TOOLS`] check below, mirroring [`CHAT_TOOLS`] /
/// [`EVENTS_TOOLS`] exactly. Kept as a (currently empty) named constant
/// rather than deleted outright so a future reader-unsupported-always
/// tool has an obvious place to land, and so the dynamic-gate doc
/// comments below can keep referring to it by name.
const UNSUPPORTED_ON_REPLICA_TOOLS: &[&str] = &[];

/// Tool surfaces a Ducklake reader may serve only when the current indexer
/// has the matching shared backend attached, paired with the probe that
/// answers "is it attached?".
///
/// One row per surface. Adding a fifth is a line here, not a fourth copy of
/// an `if state.reader_mode && ...` block — see the loop in
/// [`dispatch_tools_call`].
///
/// The pair is named rather than written inline so the constant reads as
/// "a list of gates" instead of a nested tuple type.
type SharedSurfaceProbe = (schema::Surface, fn(&Indexer) -> bool);

/// Which probe answers "is this shared backend attached?" for each surface.
///
/// Only the probe lives here now. Tool MEMBERSHIP comes from each tool's own
/// `Touches` declaration in the registry, so adding a tool to a shared
/// surface is one argument at its definition rather than a name remembered
/// into a list in this file.
const SHARED_SURFACE_PROBES: &[SharedSurfaceProbe] = &[
    (schema::Surface::Chat, Indexer::has_shared_chat),
    (schema::Surface::Events, Indexer::has_shared_events),
    (schema::Surface::Drafts, Indexer::has_shared_drafts),
    (schema::Surface::Crdt, Indexer::has_shared_crdt),
];

// Eight inputs describe one authenticated call (who, for whom, for which run);
// bundling them would only move the count somewhere less legible.
#[allow(clippy::too_many_arguments)]
async fn dispatch_tools_call(
    state: &crate::server::AppState,
    tenant_id: &str,
    role: Option<Role>,
    subject: &str,
    token_groups: &[String],
    actor: Option<&str>,
    run: Option<&escurel_auth::RunClaims>,
    agent_skill: Option<&str>,
    params: Value,
) -> Result<Value, JsonRpcError> {
    let params: ToolsCallParams = serde_json::from_value(params)
        .map_err(|e| JsonRpcError::invalid_params(format!("tools/call params: {e}")))?;

    // Ducklake-reader gate (DuckLake PR 6): reject the mutating tool
    // surface and the chat/CRDT/session/event tool surface EARLY —
    // before any tool-specific handler runs, before the indexer/session
    // routing below — with a typed error naming which bucket applies.
    let touches = schema::touches_of(&params.name);
    if state.reader_mode {
        if touches.is_some_and(|t| t.writes_index) {
            return Err(JsonRpcError::read_only_replica(params.name.clone()));
        }
        if UNSUPPORTED_ON_REPLICA_TOOLS.contains(&params.name.as_str()) {
            return Err(JsonRpcError::unsupported_on_replica(params.name.clone()));
        }
    }

    // Capture the CURRENT indexer ONCE at dispatch entry (hot-swap
    // seam, `IndexerHandle`): the whole tool call runs against one
    // consistent indexer even if a snapshot adoption swaps mid-flight.
    let current_indexer: Option<Arc<Indexer>> = state.indexer.as_ref().map(IndexerHandle::current);

    // Dynamic shared-surface gates (DuckLake PRs 8-10): a reader rejects
    // chat, events and CRDT tools UNLESS the current indexer has the matching
    // shared backend attached. Checked against the SAME captured indexer the
    // rest of this call runs against, so a hot-swap mid-flight cannot
    // disagree with itself. Every non-reader deployment (single-file, or a
    // ducklake writer) is completely unaffected — this loop is inert there.
    //
    // These were three copied blocks whose comments each said they "mirror
    // the chat gate above exactly". They did, which is why they are a table:
    // a fourth shared surface is now one row rather than a fourth copy, and
    // the three cannot drift apart while claiming not to.
    if state.reader_mode
        && let Some(surface) = touches.and_then(|t| t.surface)
    {
        let attached = SHARED_SURFACE_PROBES
            .iter()
            .find(|(s, _)| *s == surface)
            .is_some_and(|(_, probe)| current_indexer.as_deref().is_some_and(probe));
        if !attached {
            return Err(JsonRpcError::unsupported_on_replica(params.name.clone()));
        }
    }

    // Deterministic per-instance ACL caller (escurel-index). The admin
    // role bypasses owner-visibility; a missing role is dev/on-host mode
    // (no verifier, open gateway) and likewise bypasses — there is no
    // subject to scope against. A real Agent token is enforced.
    // `token_groups` are the RBAC groups from the JWT (admin-value already
    // stripped by the caller in `mcp_inner`). Built BEFORE the session-tool
    // routing below: `open_session` / `close_session` enforce the same
    // write policy `update_page` does, so they need the caller too.
    let caller = AclCaller {
        subject,
        is_admin: matches!(role, None | Some(Role::Admin)),
        token_groups,
        actor,
        run_id: run.map(|r| r.run_id.as_str()),
        root_event_id: run.and_then(|r| r.root_event_id.as_deref()),
        agent_skill,
    };

    // The admin gate, once, from the registry.
    //
    // This was `require_admin(role)?;` written as the first statement of 40
    // dispatch arms, with a ratchet test parsing the source of this function
    // to check that the set of arms containing that call matched the set of
    // tools advertising `scope: "admin"`. Two places to state one fact, and a
    // test whose job was to notice when they disagreed.
    //
    // `admin_scope_tools()` already derives the set from the registry — the
    // quota path has consulted it for exactly this since the prefix-matching
    // version of that list "silently forgot every unprefixed admin tool".
    // Now dispatch consults it too, so `Scope` at the definition site is the
    // only place a tool's audience is declared.
    //
    // Placed HERE deliberately: after the reader-replica and shared-surface
    // gates and after `caller`, and before both dispatch matches. That is the
    // order the per-arm calls produced, so which refusal a caller sees when
    // several apply is unchanged.
    if schema::admin_scope_tools().contains(params.name.as_str()) {
        require_admin(role)?;
    }

    // Session tools depend on `crdt_backend` + `sessions`, not on
    // the indexer. Route them before the indexer gate.
    match params.name.as_str() {
        "open_session" => {
            return tool_open_session(
                state.crdt_backend.as_ref(),
                current_indexer.as_deref(),
                Arc::clone(&state.sessions),
                state.quota.as_ref(),
                tenant_id,
                caller,
                state.write_acl,
                params.arguments,
            )
            .await;
        }
        "apply_op" => {
            return tool_apply_op(
                state,
                current_indexer.as_deref(),
                Arc::clone(&state.sessions),
                caller,
                subject,
                params.arguments,
            )
            .await;
        }
        "close_session" => {
            return tool_close_session(
                state,
                current_indexer.as_deref(),
                Arc::clone(&state.sessions),
                subject,
                caller,
                params.arguments,
            )
            .await;
        }
        // Admin-gated tenant CRUD + long-running ops. These take
        // `state` directly (tenant_store / indexer / crdt_backend /
        // embedder seam) rather than the bound indexer, so they route
        // before the indexer gate, mirroring the session tools above.
        "tenant_create" => {
            return tool_tenant_create(state, params.arguments).await;
        }
        "tenant_list" => {
            return tool_tenant_list(state).await;
        }
        "tenant_get" => {
            return tool_tenant_get(state, params.arguments).await;
        }
        "tenant_update" => {
            return tool_tenant_update(state, params.arguments).await;
        }
        "tenant_delete" => {
            return tool_tenant_delete(state, params.arguments).await;
        }
        "tenant_export" => {
            return tool_tenant_export(state, params.arguments).await;
        }
        "export_pack" => {
            return tool_export_pack(state, params.arguments).await;
        }
        "import_pack" => {
            return tool_import_pack(state, params.arguments).await;
        }
        "list_packs" => {
            return tool_list_packs(state).await;
        }
        "rebase_pack" => {
            return tool_rebase_pack(state, params.arguments).await;
        }
        "unsubscribe_pack" => {
            return tool_unsubscribe_pack(state, params.arguments).await;
        }
        "submit_promotion" => {
            return tool_submit_promotion(state, subject, params.arguments).await;
        }
        "tenant_import" => {
            return tool_tenant_import(state, params.arguments).await;
        }
        "attach_external" => {
            return tool_attach_external(state, params.arguments).await;
        }
        "embedding_reload" => {
            return tool_embedding_reload(state).await;
        }
        "rebuild" => {
            return tool_rebuild(state, params.arguments).await;
        }
        "compact_lanes" => {
            return tool_compact_lanes(state, params.arguments).await;
        }
        "publish_snapshot" => {
            return tool_publish_snapshot(state).await;
        }
        // Outbound-webhook delivery log (observability). Needs only the
        // webhook handle on AppState, so it routes before the indexer gate.
        "admin_webhook_deliveries" => {
            return tool_admin_webhook_deliveries(state, params.arguments);
        }
        _ => {}
    }

    let indexer = current_indexer.as_ref().ok_or_else(|| {
        JsonRpcError::internal("server has no indexer wired; tools/call is unavailable")
    })?;

    match params.name.as_str() {
        "list_skills" => tool_list_skills(indexer, caller).await,
        "list_instances" => tool_list_instances(indexer, caller, params.arguments).await,
        "get_operation" => tool_get_operation(indexer, caller, params.arguments).await,
        "resolve" => tool_resolve(indexer, caller, params.arguments).await,
        "expand" => tool_expand(state, indexer, caller, params.arguments).await,
        "fetch_blob" => tool_fetch_blob(indexer, caller, params.arguments).await,
        "neighbours" => tool_neighbours(indexer, caller, params.arguments).await,
        "provenance_ancestry" => tool_provenance_ancestry(indexer, caller, params.arguments).await,
        "provenance_report" => tool_provenance_report(indexer, caller, params.arguments).await,
        "search" => tool_search(indexer, caller, params.arguments).await,
        // A parameterized read over ONE sql_view instance's view — an
        // agent-surface tool: the per-instance ACL gates the target instance
        // (the data), so it is not admin-gated (issue #205). The legacy
        // corpus-wide `run_stored_query` twin was removed after its
        // deprecation (2026-08-14 API review, minimalism finding 3).
        "query_instance" => tool_query_instance(indexer, caller, params.arguments).await,
        "validate" => tool_validate(indexer, params.arguments).await,
        // Held writes (the `autonomy: review` gate). Agent-shaped like the
        // event surface, and write-ACL'd at CREATE — see `tools_drafts`.
        "create_draft" => {
            tool_create_draft(state, indexer, caller, state.write_acl, params.arguments).await
        }
        "list_drafts" => tool_list_drafts(indexer, caller, params.arguments).await,
        "create_branch" => tool_create_branch(state, indexer, caller, params.arguments).await,
        "list_branches" => tool_list_branches(indexer, caller, params.arguments).await,
        "merge_branch" => {
            tool_merge_branch(state, indexer, caller, state.write_acl, params.arguments).await
        }
        "abandon_branch" => tool_abandon_branch(indexer, caller, params.arguments).await,
        "diff_draft" => tool_diff_draft(indexer, caller, params.arguments).await,
        "list_changesets" => tool_list_changesets(indexer, caller, params.arguments).await,
        "promote_changeset" => {
            tool_promote_changeset(state, indexer, caller, state.write_acl, params.arguments).await
        }
        "discard_changeset" => {
            tool_discard_changeset(state, indexer, caller, params.arguments).await
        }
        "promote_draft" => {
            tool_promote_draft(state, indexer, caller, state.write_acl, params.arguments).await
        }
        "discard_draft" => tool_discard_draft(state, indexer, caller, params.arguments).await,
        "update_page" => {
            tool_update_page(state, indexer, caller, state.write_acl, params.arguments).await
        }
        "delete_page" => {
            tool_delete_page(state, indexer, caller, state.write_acl, params.arguments).await
        }
        "move_page" => {
            tool_move_page(state, indexer, caller, state.write_acl, params.arguments).await
        }
        "purge_page" => {
            // Destroys the audit record `delete_page` retained — an operator
            // act. Admin-gated like the other audit-destroying tools; the
            // owner of a page may retract it, but not erase the husk.
            tool_purge_page(state, indexer, params.arguments).await
        }
        "append_message" => {
            tool_append_message(indexer, caller, state.write_acl, params.arguments).await
        }
        "list_messages" => {
            tool_list_messages(indexer, caller, state.write_acl, params.arguments).await
        }
        // Event-bus surface. Agent-shaped, so not admin-gated — but it
        // carries the `AclCaller` for the same reason the instance reads
        // do: an inbox event is unreviewed third-party text, and a shared
        // tenant must not let one caller read or claim another's captures.
        // `capture_event` uses the caller to STAMP the event's owner; the
        // other three use it to FILTER per row (`may_read_event`), under
        // `ESCUREL_EVENT_ACL`.
        "capture_event" => {
            tool_capture_event(
                indexer,
                caller,
                state.event_acl,
                state.write_acl,
                state.webhook.as_ref(),
                &state.events_tx,
                params.arguments,
            )
            .await
        }
        "start_operation" => {
            tool_start_operation(
                indexer,
                caller,
                state.webhook.as_ref(),
                &state.events_tx,
                state.operation_slug_secret.as_deref(),
                params.arguments,
            )
            .await
        }
        "list_inbox" => tool_list_inbox(indexer, caller, state.event_acl, params.arguments).await,
        "report_progress" => {
            tool_report_progress(
                indexer,
                caller,
                &state.events_tx,
                state.run_progress_keep,
                params.arguments,
            )
            .await
        }
        "mint_agent_token" => {
            tools_mint::tool_mint_agent_token(state, indexer, caller, params.arguments).await
        }
        "get_run_tool_calls" => {
            tools_tool_calls::tool_get_run_tool_calls(
                indexer,
                caller,
                state.event_acl,
                params.arguments,
            )
            .await
        }
        "list_lineage" => {
            tool_list_lineage(indexer, caller, state.event_acl, params.arguments).await
        }
        "list_events" => tool_list_events(indexer, caller, state.event_acl, params.arguments).await,
        "list_snapshots" => tool_list_snapshots(indexer, caller, params.arguments).await,
        "list_op_authors" => {
            tool_list_op_authors(
                state.crdt_backend.as_ref(),
                indexer,
                caller,
                params.arguments,
            )
            .await
        }
        "assign_event" => {
            tool_assign_event(indexer, caller, state.event_acl, params.arguments).await
        }
        // Admin-gated ops tools (mirror the documented MCP admin
        // surface; delegate to the same logic as EscurelAdmin gRPC).
        "admin_quota" => tool_admin_quota(state, tenant_id, params.arguments),
        "admin_audit" => tool_admin_audit(indexer, params.arguments).await,
        "admin_index_query" => tool_admin_index_query(indexer, params.arguments).await,
        "admin_delete_chat_history" => {
            tool_admin_delete_chat_history(indexer, params.arguments).await
        }
        "admin_list_lanes" => tool_admin_list_lanes(indexer),
        "admin_lane_keys" => tool_admin_lane_keys(indexer, params.arguments).await,
        "admin_lane_blob" => tool_admin_lane_blob(indexer, params.arguments).await,
        // Group ACL v1: admin-only membership mutation + read (D14). Gated
        // here, exactly like the other operator tools; group membership is
        // the source of truth for custom-group RBAC.
        "add_group_member" => tool_add_group_member(indexer, subject, params.arguments).await,
        "remove_group_member" => tool_remove_group_member(indexer, params.arguments).await,
        "list_group_members" => tool_list_group_members(indexer, params.arguments).await,
        // SQL-view credential registry (admin-only). Secrets live
        // server-side in kb.duckdb, never in the markdown corpus (REQ-SQL-05).
        "register_credential" => tool_register_credential(indexer, subject, params.arguments).await,
        "list_credentials" => tool_list_credentials(indexer).await,
        "delete_credential" => tool_delete_credential(indexer, params.arguments).await,
        "validate_bindings" => tool_validate_bindings(indexer).await,
        "create_sql_instance" => tool_create_sql_instance(indexer, params.arguments).await,
        // Remote-backend endpoint registry (admin-only). Base URL + auth live
        // server-side in kb.duckdb; the secret is never echoed. This is the
        // SSRF guard — a remote instance can only reach a registered endpoint.
        "register_endpoint" => tool_register_endpoint(indexer, subject, params.arguments).await,
        "list_endpoints" => tool_list_endpoints(indexer).await,
        "delete_endpoint" => tool_delete_endpoint(indexer, params.arguments).await,
        "validate_endpoints" => tool_validate_endpoints(indexer).await,
        // Materialise a remote (openapi/mcp) overlay page from a skill that
        // declares a remote backend. Admin-only, mirroring create_sql_instance.
        "create_remote_instance" => tool_create_remote_instance(indexer, params.arguments).await,
        // Write-back to a remote instance's upstream. Agent tool, gated by the
        // target instance's acl.update (may_write_instance, fail-closed).
        "write_instance" => tool_write_instance(indexer, caller, params.arguments).await,
        other => Err(JsonRpcError::method_not_found(format!(
            "unknown tool `{other}`"
        ))),
    }
}

// --- session tools (M4.2) --------------------------------------

#[derive(Deserialize)]
struct OpenSessionArgs {
    page_id: Option<String>,
    /// A live PERSONAL DRAFT to edit instead of a page (SPEC §7 PR-1). The
    /// session's ops mutate the held bytes; the target page does not move
    /// until the draft is promoted, which keeps every guard the draft already
    /// has. Exclusive with `page_id`.
    draft_id: Option<String>,
}

/// Whether `caller` may WRITE the page a session targets — the SAME policy
/// `update_page` enforces (`may_write_page`), applied to the session
/// surface so live co-authoring cannot route around the write ACL.
///
/// Two checks, both fail-closed on a denial:
///
/// * the **stored page** (the `move_page`/`delete_page` shape): may this
///   caller overwrite what is there? A page that does not exist yet has no
///   ACL to consult and passes — the create is then gated by…
/// * the **incoming content**, when one is supplied (the commit body at
///   `close_session`) and it parses as a page: the `update_page` shape,
///   which also catches a create-owned-by-someone-else. A body that does
///   not parse is left to `update_page_as` itself, which refuses it the
///   same way it always has.
async fn session_write_allowed(
    ix: &Indexer,
    caller: &AclCaller<'_>,
    page_id: &str,
    incoming: Option<&str>,
) -> Result<bool, JsonRpcError> {
    if let Some(existing) = ix
        .read_page_markdown(page_id)
        .await
        .map_err(|e| JsonRpcError::internal(format!("session acl read: {e}")))?
        && !ix
            .may_write_page(caller, page_id, &existing)
            .await
            .map_err(|e| JsonRpcError::internal(format!("session acl: {e}")))?
    {
        return Ok(false);
    }
    if let Some(incoming) = incoming.filter(|c| escurel_md::parse(c).is_ok())
        && !ix
            .may_write_page(caller, page_id, incoming)
            .await
            .map_err(|e| JsonRpcError::internal(format!("session acl: {e}")))?
    {
        return Ok(false);
    }
    Ok(true)
}

/// The draft a session may be opened on, or a refusal.
///
/// A held draft is PERSONAL until it is promoted (SPEC §7): its author is the
/// only one who may edit it, admin aside. That is stricter than the page ACL
/// `list_drafts` reads, and deliberately so — a colleague who may write the
/// target page still has no business typing into someone's unfinished work.
///
/// Absence and denial are ONE answer. "No such draft" and "not yours" must
/// read identically, or the refusal becomes an oracle for what other people
/// are drafting. A decided draft is the exception: the caller already knows it
/// exists, so `already_decided` is the honest answer and the useful one.
async fn open_draft_for_session(
    ix: &Indexer,
    caller: &AclCaller<'_>,
    draft_id: &str,
) -> Result<escurel_index::drafts::DraftInfo, JsonRpcError> {
    let denied = || {
        JsonRpcError {
            code: -32000,
            message: "open_session: no such draft, or not yours to edit".to_owned(),
            data: None,
        }
        .with_code("forbidden", false)
    };
    let draft = ix
        .get_draft(draft_id)
        .await
        .map_err(|e| JsonRpcError::internal(format!("open_session draft: {e}")))?
        .ok_or_else(denied)?;
    if !caller.is_admin && draft.author != caller.subject {
        return Err(denied());
    }
    if draft.status != "open" {
        return Err(JsonRpcError {
            code: -32000,
            message: format!(
                "open_session: draft `{draft_id}` was already {} — there is nothing left to edit",
                draft.status
            ),
            data: None,
        }
        .with_code("already_decided", false));
    }
    Ok(draft)
}

#[allow(clippy::too_many_arguments)]
async fn tool_open_session(
    backend: Option<&Arc<dyn CrdtBackend>>,
    indexer: Option<&Indexer>,
    sessions: Arc<SessionManager>,
    quota: Option<&Arc<QuotaManager>>,
    tenant_id: &str,
    caller: AclCaller<'_>,
    write_acl: crate::server::WriteAclMode,
    args: Value,
) -> Result<Value, JsonRpcError> {
    let a: OpenSessionArgs = parse_args(args, "open_session")?;
    let backend = backend
        .ok_or_else(|| JsonRpcError::internal("live CRDT mode not enabled on this server"))?;

    // A session edits a page or a draft, never both and never neither: the two
    // commit to different places, so an ambiguous target has no safe reading.
    let (page_id, draft) = match (&a.page_id, &a.draft_id) {
        (Some(_), Some(_)) => {
            return Err(JsonRpcError::invalid_params(
                "open_session: name `page_id` or `draft_id`, not both",
            ));
        }
        (None, None) => {
            return Err(JsonRpcError::invalid_params(
                "open_session: name the page (`page_id`) or the draft (`draft_id`) to edit",
            ));
        }
        (Some(page_id), None) => (page_id.clone(), None),
        (None, Some(draft_id)) => {
            let ix = indexer.ok_or_else(|| {
                JsonRpcError::internal("open_session: drafts need an indexed corpus")
            })?;
            let draft = open_draft_for_session(ix, &caller, draft_id).await?;
            (draft.target_page_id.clone(), Some(draft))
        }
    };

    // Base-layer guard (REQ-LAYER-02): live co-authoring must not bypass
    // the `update_page` read-only guard — an open session's `apply_op`
    // stream would edit a base page byte by byte. The reserved-prefix
    // half is static (no indexer needed) and race-free.
    if page_id.starts_with(escurel_index::pack::RESERVED_BASE_PREFIX) {
        return Err(JsonRpcError {
            code: -32000,
            message: format!(
                "layer_read_only: page `{}` is under the reserved `{}` namespace — \
                 pack-managed, read-only at this node",
                page_id,
                escurel_index::pack::RESERVED_BASE_PREFIX
            ),
            data: None,
        }
        .with_code("layer_read_only", false));
    }
    // Session-only servers (`indexer = None`) have no page corpus, so no
    // stored base pages to guard beyond the prefix above.
    if let Some(ix) = indexer {
        let layer = ix
            .page_layer(&page_id)
            .await
            .map_err(|e| JsonRpcError::internal(format!("open_session layer guard: {e}")))?;
        if let Some(layer) = layer.filter(|l| l.starts_with("base@")) {
            return Err(JsonRpcError {
                code: -32000,
                message: format!(
                    "layer_read_only: page `{}` is layer `{layer}` — imported from a \
                     subscribed pack and read-only at this node; author an overlay \
                     page to specialise it",
                    page_id
                ),
                data: None,
            }
            .with_code("layer_read_only", false));
        }
    }

    // Write-ACL gate: an open session's `apply_op` stream edits the page
    // byte by byte, so opening one is gated by the SAME write policy
    // `update_page` enforces — a caller `update_page` would refuse must
    // not get a session instead. Mirrors the WS attach gate (#352), on
    // the write side. Session-only servers (`indexer = None`) have no
    // page corpus, so no ACL exists to enforce.
    // A session on a page that does not exist has no policy behind it, and
    // the gate below reads the STORED page to decide — so with nothing to
    // read it decides nothing and allows. #410 closed the case where the page
    // existed and the caller was outside its ACL; this is the same fail-open
    // reached through absence, and it is not hypothetical: the caller gets a
    // session id, `head_version: "v0"` and a `/ws` URL for a page nobody
    // wrote. Refusal is the only honest answer, and it must not distinguish
    // "no such page" from "not yours" — the two are one message on purpose.
    //
    // Session-only servers (`indexer = None`) keep their behaviour: they have
    // no page corpus, so absence is their normal state rather than a gap.
    if write_acl != crate::server::WriteAclMode::Off
        && let Some(ix) = indexer
        && ix
            .read_page_markdown(&page_id)
            .await
            .map_err(|e| JsonRpcError::internal(format!("open_session existence: {e}")))?
            .is_none()
    {
        return Err(JsonRpcError {
            code: -32000,
            message: format!(
                "open_session denied: caller `{}` may not open a session on `{}`",
                caller.subject, page_id
            ),
            data: None,
        }
        .with_code("forbidden", false));
    }

    if write_acl != crate::server::WriteAclMode::Off
        && let Some(ix) = indexer
        && !session_write_allowed(ix, &caller, &page_id, None).await?
    {
        if write_acl == crate::server::WriteAclMode::Log {
            tracing::warn!(
                subject = %caller.subject,
                page_id = %page_id,
                "write-ACL would deny this open_session (log mode) — allowing"
            );
        } else {
            return Err(JsonRpcError {
                code: -32000,
                message: format!(
                    "open_session denied: caller `{}` does not own instance `{}`",
                    caller.subject, page_id
                ),
                data: None,
            }
            .with_code("forbidden", false));
        }
    }

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
                    data: None,
                }
                .with_code("session_cap_reached", true));
            }
        }
    } else {
        None
    };

    // The page's stored markdown, so a session over a page that has content
    // starts FROM that content (#421). Read here rather than inside the
    // session manager because this is the layer that has the indexer, and a
    // session-only server (`indexer = None`) genuinely has no page to seed
    // from — its documents are the whole story.
    // A draft session starts from the DRAFT's held bytes, not the page's: the
    // point of editing a draft is to carry on from what was proposed.
    let seed = match (&draft, indexer) {
        (Some(d), _) => Some(d.content.clone()),
        (None, Some(ix)) => ix
            .read_page_markdown(&page_id)
            .await
            .map_err(|e| JsonRpcError::internal(format!("open_session seed: {e}")))?,
        (None, None) => None,
    };

    // The session key: the draft when there is one, so its document and its
    // one-session-per-target reservation stay separate from the page's.
    let key = match &draft {
        Some(d) => crate::session::draft_key(&d.draft_id),
        None => page_id.clone(),
    };

    let (session_id, head) = sessions
        .open(
            Arc::clone(backend),
            &key,
            guard,
            seed.as_deref(),
            caller.subject,
        )
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

/// `apply_op`'s arguments. Note what is NOT here: there is no way for a
/// caller to name the op's author. The principal is taken from the verified
/// token below, and an unknown field in `arguments` is ignored rather than
/// read (#357) — the only forgery-proof shape.
#[derive(Deserialize)]
struct ApplyOpArgs {
    session: String,
    op: String,
}

async fn tool_apply_op(
    state: &crate::server::AppState,
    indexer: Option<&Indexer>,
    sessions: Arc<SessionManager>,
    caller: AclCaller<'_>,
    subject: &str,
    args: Value,
) -> Result<Value, JsonRpcError> {
    let a: ApplyOpArgs = parse_args(args, "apply_op")?;
    if state.crdt_backend.is_none() {
        return Err(JsonRpcError::internal(
            "live CRDT mode not enabled on this server",
        ));
    }

    // The same gate `close_session` applies, for the same reason and at the
    // same bar `open_session` set: admin passes, the opener passes, anyone
    // else must be able to write the page.
    //
    // This had NO check at all. The handler took the subject only to stamp
    // authorship and then applied the op, so a session id was a bearer
    // capability over the page it belongs to — and session ids travel in
    // tool results and logs. Both ends of a session were gated and the
    // middle, which is the part that actually edits bytes, was not.
    if !caller.is_admin && sessions.opened_by(&a.session).as_deref() != Some(subject) {
        let permitted = match (sessions.page_id_of(&a.session), indexer) {
            // A DRAFT session is personal: only its opener (its author) and
            // admin may type into it. The page-write fallback below must not
            // apply — a colleague who may write the target page still has no
            // business editing someone's unfinished draft, and a draft key
            // names no page, so that fallback would have allowed anyone
            // holding the session id.
            (Some(key), _) if crate::session::draft_id_of_key(&key).is_some() => false,
            (Some(page_id), Some(ix)) => {
                state.write_acl == crate::server::WriteAclMode::Off
                    || session_write_allowed(ix, &caller, &page_id, None).await?
            }
            // No page behind it, or an unknown session: nothing to protect,
            // and an unknown id must not read differently from a forbidden
            // one. Mirrors `close_session`.
            _ => true,
        };
        if !permitted {
            return Err(JsonRpcError {
                code: -32000,
                message: format!(
                    "apply_op denied: caller `{subject}` neither opened session \
                     `{}` nor may write its page",
                    a.session
                ),
                data: None,
            }
            .with_code("forbidden", false));
        }
    }

    let op_bytes = B64
        .decode(a.op.as_bytes())
        .map_err(|e| JsonRpcError::invalid_params(format!("apply_op `op` is not base64: {e}")))?;
    // The op author is the verified subject, not the Loro peer id inside
    // `op_bytes` — that identifies a device, and two people sharing one
    // browser tab are two authors (#357 / CR-6).
    let mut op = Op::new(op_bytes);
    if let Some(p) = tools_write::stamped_principal(subject) {
        op = op.by(p);
    }
    let merged = sessions
        .apply(&a.session, op)
        .await
        .map_err(|e| session_error_to_jsonrpc(&e, "apply_op"))?;
    Ok(json!({
        "ok": true,
        "merged_version": merged.as_str(),
    }))
}

#[derive(Deserialize)]
struct ListOpAuthorsArgs {
    page_id: String,
}

/// `list_op_authors`: who wrote each CRDT op on a page, oldest first.
///
/// The read path for `crdt_ops.principal` (#357 / CR-6). Deliberately not
/// part of `expand`: op history is unbounded, and a page read should not
/// carry it. Deliberately not the op BYTES either — this answers "who edited
/// this, and when", which is the audit question, without republishing the
/// document's edit payloads to anyone who can name the page.
///
/// The ops themselves live wherever the CRDT backend points (the local
/// `crdt_ops` table, or the shared attached-Postgres one) — which the backend
/// knows and the indexer does not. But the ACL decision needs the indexed
/// page, so this routes with the indexer tools and takes both. On a gateway
/// with no indexer the tool is unavailable rather than ungated: there is
/// nothing to make the decision against, and failing closed is the only
/// answer that cannot leak.
///
/// ## The gate
///
/// Authorship is metadata ABOUT the page, so it follows the page's own read
/// ACL — `may_read_instance`, the same predicate `expand` / `search` /
/// `list_instances` consult, rather than a second notion of "may read" that
/// could drift from them. Skill pages are the public catalogue and are never
/// gated, exactly as in `tool_expand`.
///
/// Denial is **absence, not error**: a refusal that said "this page exists
/// and you may not see it" would be an existence oracle, so a denied caller
/// gets the empty history a page with no ops returns. The refusal is
/// therefore byte-identical to the truthful answer for a page that is not
/// there — which is the property `list_op_authors_denial_reads_as_absence…`
/// pins.
///
/// (`open_session` on an arbitrary page is NOT gated today. That is
/// pre-existing and belongs to a deliberate pass over the whole session-tool
/// surface; an existing hole is not a licence to add another.)
async fn tool_list_op_authors(
    backend: Option<&Arc<dyn CrdtBackend>>,
    indexer: &Indexer,
    caller: AclCaller<'_>,
    args: Value,
) -> Result<Value, JsonRpcError> {
    let a: ListOpAuthorsArgs = parse_args(args, "list_op_authors")?;
    let Some(backend) = backend else {
        return Err(JsonRpcError::internal(
            "live CRDT mode not enabled on this server",
        ));
    };
    // A page_id with no indexed page has no ACL to consult and nothing to
    // protect — a bare CRDT page id, or a page that genuinely does not
    // exist. Both fall through to the empty history below.
    let readable = match indexer
        .expand(&a.page_id, None, None)
        .await
        .map_err(|e| JsonRpcError::internal(format!("list_op_authors acl: {e}")))?
    {
        Some(e) if e.page.page_type == PageType::Instance => indexer
            .may_read_instance(&caller, &e.page.skill, &e.frontmatter)
            .await
            .map_err(|e| JsonRpcError::internal(format!("list_op_authors acl: {e}")))?,
        _ => true,
    };
    let authors = if readable {
        backend
            .op_authors(&a.page_id)
            .await
            .map_err(|e| JsonRpcError::internal(format!("list_op_authors: {e}")))?
    } else {
        Vec::new()
    };
    Ok(json!({
        "page_id": a.page_id,
        "ops": authors.iter().map(|o| json!({
            "op_id": o.op_id,
            "hlc": o.hlc,
            "applied_at": o.applied_at,
            "principal": o.principal,
        })).collect::<Vec<_>>(),
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

/// `close_session`: end a live editing session and, on commit, **write the
/// merged body through to the indexer**.
///
/// ## Why the commit writes through (F1, Option A)
///
/// This used to call `sessions.close` and nothing else, which wrote only a
/// CRDT snapshot. `expand` composes its reply from two stores — `body` from
/// the indexer, `version` from `backend.max_hlc` — so a committed session
/// advanced the version a client reads while leaving the body it reads
/// stale. A well-behaved client that read that pair, edited, and wrote back
/// with the `base_version` it was handed took the `base == head` path, no
/// merge was attempted, and the committed session edits were overwritten.
/// That is silent data loss, and no client error could avoid it.
///
/// Hydrating `expand` from the snapshot instead would have fixed the symptom
/// and left the disease: the indexer also owns `blocks` (which feed BM25 and
/// the vector index) and `links` (which feed `neighbours` and backlinks), so
/// search still could not have found a committed edit. See
/// `docs/notes/concurrency-fix-plan.md` F1.
///
/// ## Ordering under failure
///
/// The indexer write happens **first** and the CRDT snapshot **last**:
///
/// 1. read the merged body out of the still-open session,
/// 2. take `update_page_gate` and write the indexer,
/// 3. only then `sessions.close(commit)`, which snapshots.
///
/// A failing indexer write therefore leaves the session **open and
/// retryable** rather than half-applied. The reverse order would strand a
/// committed snapshot that nothing reconciles — `escurel-crdt`'s reconciler
/// solves the opposite direction and is not wired into the server at all.
///
/// `commit = false` is a discard and still writes nothing.
async fn tool_close_session(
    state: &crate::server::AppState,
    indexer: Option<&Indexer>,
    sessions: Arc<SessionManager>,
    subject: &str,
    caller: AclCaller<'_>,
    args: Value,
) -> Result<Value, JsonRpcError> {
    let a: CloseSessionArgs = parse_args(args, "close_session")?;
    if state.crdt_backend.is_none() {
        return Err(JsonRpcError::internal(
            "live CRDT mode not enabled on this server",
        ));
    }

    // Read the merged body BEFORE closing — `close` removes the entry, after
    // which neither the content nor the page id is reachable.
    let write_through = if a.commit {
        match (sessions.page_id_of(&a.session), indexer) {
            (Some(page_id), Some(ix)) => sessions
                .current_content(&a.session)
                .await
                .map(|body| (page_id, ix, body)),
            _ => None,
        }
    } else {
        None
    };

    // **Discard is gated too, but on identity rather than the write ACL
    // (#425).** The commit path re-checks the write policy below. Discard
    // deliberately does not, so a caller whose grant was revoked mid-session
    // can still abandon their own work instead of wedging the page until the
    // idle TTL — but that exemption has to know WHOSE work it is. Without this
    // it protected anyone holding the session id, and a caller on another
    // engagement could terminate a live workshop in a room they have no
    // relationship to. No content is read, so it is not a disclosure; it is a
    // cross-engagement denial of service.
    //
    // Admin passes, as everywhere. The opener passes. Anyone else must be able
    // to write the page, which is the same bar `open_session` set.
    if !a.commit && !caller.is_admin && sessions.opened_by(&a.session).as_deref() != Some(subject) {
        let permitted = match (sessions.page_id_of(&a.session), indexer) {
            // A draft session is personal (see `apply_op`): abandoning someone
            // else's draft is not a page write anyone can inherit.
            (Some(key), _) if crate::session::draft_id_of_key(&key).is_some() => false,
            (Some(page_id), Some(ix)) => {
                state.write_acl == crate::server::WriteAclMode::Off
                    || session_write_allowed(ix, &caller, &page_id, None).await?
            }
            // No page behind it (session-only server) or an unknown session:
            // nothing to protect, and an unknown id must not read differently
            // from a forbidden one.
            _ => true,
        };
        if !permitted {
            return Err(JsonRpcError {
                code: -32000,
                message: format!(
                    "close_session denied: caller `{subject}` neither opened \
                     session `{}` nor may write its page",
                    a.session
                ),
                data: None,
            }
            .with_code("forbidden", false));
        }
    }

    if let Some((key, ix, body)) = write_through.as_ref().and_then(|(k, ix, b)| {
        crate::session::draft_id_of_key(k).map(|d| (d.to_owned(), *ix, b.clone()))
    }) {
        // A draft session commits to the DRAFT ROW. The target page must not
        // move: that is what promotion is for, and it is where the CAS, the
        // write ACL and `already_decided` live. Empty is a no-op for the same
        // reason as below.
        if !body.trim().is_empty() {
            // The author gate is re-checked here for the same reason the page
            // path re-checks the write ACL: the commit is the write, and the
            // draft may have been decided while the session was open.
            let permitted = caller.is_admin
                || ix
                    .get_draft(&key)
                    .await
                    .map_err(|e| JsonRpcError::internal(format!("close_session draft: {e}")))?
                    .is_some_and(|d| d.author == subject);
            if !permitted {
                return Ok(json!({
                    "ok": false,
                    "issues": [{
                        "severity": "error",
                        "code": "forbidden",
                        "location": "frontmatter",
                        "message": format!(
                            "write denied: caller `{subject}` is not the author of draft `{key}`"
                        ),
                    }],
                }));
            }
            if ix
                .set_draft_content(&key, &body)
                .await
                .map_err(|e| JsonRpcError::internal(format!("close_session draft write: {e}")))?
                .is_none()
            {
                // Decided under the session. The work is not landable any
                // more, and saying so beats a silent discard.
                return Ok(json!({
                    "ok": false,
                    "issues": [{
                        "severity": "error",
                        "code": "already_decided",
                        "location": "frontmatter",
                        "message": format!(
                            "draft `{key}` was decided while the session was open; \
                             its edits were not saved"
                        ),
                    }],
                }));
            }
        }
    } else if let Some((page_id, ix, body)) = write_through {
        // Empty is what a just-opened session that never received an op
        // reports. Writing it would blank the page, so a no-op session stays
        // a no-op.
        if !body.trim().is_empty() {
            // Re-check the write ACL at COMMIT time, under the same policy
            // `update_page` enforces. The open-time gate is not enough: the
            // ACL can change while a session is open (an ownership
            // transfer, a revoked grant), and the commit is the write. A
            // refusal uses `update_page`'s own denial shape and leaves the
            // session OPEN — the caller can still discard (`commit:
            // false`), mirroring the failing-indexer-write contract below.
            if state.write_acl != crate::server::WriteAclMode::Off
                && !session_write_allowed(ix, &caller, &page_id, Some(&body)).await?
            {
                if state.write_acl == crate::server::WriteAclMode::Log {
                    tracing::warn!(
                        subject = %caller.subject,
                        page_id = %page_id,
                        "write-ACL would deny this close_session commit (log mode) — allowing"
                    );
                } else {
                    return Ok(json!({
                        "ok": false,
                        "issues": [{
                            "severity": "error",
                            "code": "forbidden",
                            "location": "frontmatter",
                            "message": format!(
                                "write denied: caller `{}` does not own instance `{}`",
                                caller.subject, page_id
                            ),
                        }],
                    }));
                }
            }
            let _gate = state.update_page_gate.lock().await;
            // The commit is a page write, so it carries the same stamp an
            // `update_page` would (#357): the caller that closed the
            // session is the page's last writer. Per-op authorship stays in
            // `crdt_ops.principal`, which is what tells you the other
            // people whose edits are in this body.
            ix.update_page_as(&page_id, &body, tools_write::stamped_principal(subject))
                .await
                .map_err(|e| JsonRpcError::internal(format!("close_session write-through: {e}")))?;
        }
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
    let e = JsonRpcError::internal(format!("{tool}: {err}"));
    match err {
        // The caller's state problem, not a server fault: the session was
        // never opened or has been closed — reopen, don't back off. The
        // numeric code stays `-32603` (frozen wire contract); the data
        // code is what lets a client tell the two apart.
        SessionError::UnknownSession(_) => e.with_code("unknown_session", false),
        _ => e,
    }
}

// --- tools/list payload ----------------------------------------

/// MCP `tools/list` response payload. Each entry is `{ name,
/// description, inputSchema }`. The wire shape matches the
/// upstream MCP spec exactly so any conforming MCP client can
/// drive Escurel without bespoke wiring.
#[derive(Debug)]
struct JsonRpcError {
    code: i32,
    message: String,
    /// Machine-readable refusal detail: `{code, retryable, …}`.
    /// Additive — `code`/`message` are the frozen wire contract, and a
    /// client that ignores `data` behaves exactly as before. `data.code`
    /// is a stable string a client can branch on INSTEAD of parsing the
    /// message (which is one wording edit away from breaking them), and
    /// `data.retryable` is the flag `protocol.md` §Errors promised.
    data: Option<Value>,
}

impl JsonRpcError {
    /// Attach the machine-readable `{code, retryable}` detail.
    fn with_code(mut self, code: &str, retryable: bool) -> Self {
        self.data = Some(json!({ "code": code, "retryable": retryable }));
        self
    }
    fn method_not_found(msg: impl Into<String>) -> Self {
        Self {
            code: -32601,
            message: msg.into(),
            data: None,
        }
    }
    fn invalid_params(msg: impl Into<String>) -> Self {
        Self {
            code: -32602,
            message: msg.into(),
            data: None,
        }
    }
    fn internal(msg: impl Into<String>) -> Self {
        Self {
            code: -32603,
            message: msg.into(),
            data: None,
        }
    }
    /// The caller is authenticated but not permitted to perform the action
    /// (per-instance / chat ACL denial). App-defined code in the
    /// JSON-RPC implementation-defined server-error range.
    fn forbidden(msg: impl Into<String>) -> Self {
        Self {
            code: -32003,
            message: msg.into(),
            data: None,
        }
        .with_code("forbidden", false)
    }
    /// A precondition the server can't satisfy (e.g. an admin tool
    /// asked to act on a tenant other than the one this single-tenant
    /// gateway is bound to). Mirrors the old gRPC `FailedPrecondition`.
    fn failed_precondition(msg: impl Into<String>) -> Self {
        Self {
            code: -32002,
            message: msg.into(),
            data: None,
        }
        .with_code("failed_precondition", false)
    }
    /// A ducklake reader replica cannot serve a mutating tool — its
    /// index is a throwaway copy adopted from the lake; only the writer
    /// mutates (DuckLake PR 6). App-defined code, distinct from
    /// [`Self::unsupported_on_replica`] so a client can tell "try the
    /// writer" apart from "this surface doesn't exist here at all".
    fn read_only_replica(tool: impl Into<String>) -> Self {
        let tool = tool.into();
        Self {
            code: -32004,
            message: format!(
                "`{tool}` is unavailable: this is a read-only ducklake replica; \
                 retry against the writer instance"
            ),
            data: None,
        }
        .with_code("read_only_replica", true)
    }
    /// A ducklake reader replica has no CRDT/session backend at all
    /// (`crdt_backend: None`), and its chat/events surfaces are
    /// unsupported UNLESS the deployment's shared attached-Postgres
    /// backend is wired (`has_shared_chat`/`has_shared_events`, DuckLake
    /// PRs 8-9). Re-homing the remaining CRDT/session surface off the
    /// writer (Phase B) is PR 10, not built yet.
    fn unsupported_on_replica(tool: impl Into<String>) -> Self {
        let tool = tool.into();
        Self {
            code: -32005,
            message: format!(
                "`{tool}` is unsupported on a ducklake replica: no chat/events/CRDT \
                 backend is wired here"
            ),
            data: None,
        }
        .with_code("unsupported_on_replica", false)
    }
    /// `publish_snapshot` has no lake to publish to — the single-file
    /// backend (`ESCUREL_INDEX_BACKEND` unset or `single-file`), which
    /// never publishes snapshots at all (DuckLake PR 7). Distinct from
    /// [`Self::read_only_replica`] (a reader IS ducklake-backed, just
    /// not the writer) so a client can tell "wrong backend entirely"
    /// apart from "wrong instance of the right backend".
    fn publish_unavailable(reason: impl Into<String>) -> Self {
        Self {
            code: -32006,
            message: format!("`publish_snapshot` is unavailable: {}", reason.into()),
            data: None,
        }
        .with_code("publish_unavailable", false)
    }
    fn into_response(self, id: Value) -> axum::response::Response {
        (
            StatusCode::OK,
            Json(json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": match self.data {
                    Some(data) => {
                        json!({ "code": self.code, "message": self.message, "data": data })
                    }
                    None => json!({ "code": self.code, "message": self.message }),
                },
            })),
        )
            .into_response()
    }
}

fn error_response(id: Value, code: i32, msg: impl Into<String>) -> axum::response::Response {
    JsonRpcError {
        code,
        message: msg.into(),
        data: None,
    }
    .into_response(id)
}

#[cfg(test)]
mod search_fusion_tests {
    use super::*;

    fn args(q: Option<&str>, queries: Option<Vec<&str>>) -> SearchArgs {
        SearchArgs {
            q: q.map(str::to_owned),
            queries: queries.map(|v| v.into_iter().map(str::to_owned).collect()),
            k: 10,
            page_type: None,
            skill: None,
            as_of: None,
            scenario: None,
            granularity: None,
            filter: None,
            page_id: None,
        }
    }

    #[test]
    fn effective_queries_falls_back_to_scalar_q() {
        assert_eq!(
            effective_queries(&args(Some("hello"), None)).unwrap(),
            ["hello"]
        );
    }

    #[test]
    fn effective_queries_uses_plural_and_dedups_preserving_order() {
        let v = effective_queries(&args(None, Some(vec!["a", "b", "a", " ", "c"]))).unwrap();
        assert_eq!(
            v,
            ["a", "b", "c"],
            "blank dropped, dup 'a' removed, order kept"
        );
    }

    #[test]
    fn effective_queries_caps_variant_count() {
        let many: Vec<&str> = ["q0", "q1", "q2", "q3", "q4", "q5", "q6", "q7", "q8", "q9"].to_vec();
        let v = effective_queries(&args(None, Some(many))).unwrap();
        assert_eq!(v.len(), MAX_QUERY_VARIANTS);
    }

    #[test]
    fn effective_queries_errors_when_nothing_supplied() {
        assert!(effective_queries(&args(None, None)).is_err());
        assert!(effective_queries(&args(Some("  "), Some(vec![" "]))).is_err());
    }

    fn hit(page_id: &str) -> escurel_index::SearchHit {
        escurel_index::SearchHit {
            page_id: page_id.to_owned(),
            slug: None,
            skill: "note".to_owned(),
            page_type: PageType::Instance,
            anchor: None,
            snippet: String::new(),
            score: 0.0,
            similarity: 0.0,
            frontmatter_excerpt: json!({}),
        }
    }

    #[test]
    fn rrf_fuse_many_rewards_pages_found_by_multiple_lanes() {
        // `p_shared` is rank-0 in two lanes; `p_a`/`p_b` appear once each.
        // The page two lanes agree on must outrank the singletons.
        let lane_a = vec![hit("p_shared"), hit("p_a")];
        let lane_b = vec![hit("p_shared"), hit("p_b")];
        let fused = rrf_fuse_many(vec![lane_a, lane_b], 10);
        assert_eq!(fused[0].page_id, "p_shared");
        assert_eq!(fused.len(), 3, "exactly the union of distinct pages");
        assert!(fused.windows(2).all(|w| w[0].score >= w[1].score));
    }

    #[test]
    fn rrf_fuse_many_caps_to_requested_size() {
        let lane: Vec<_> = (0..20).map(|i| hit(&format!("p{i}"))).collect();
        let fused = rrf_fuse_many(vec![lane], 5);
        assert_eq!(fused.len(), 5);
    }
}

#[cfg(test)]
mod registry_conformance {
    //! Dispatch arms and the discovery payload must name the same tools.
    //!
    //! R2 of `docs/notes/complexity-reduction-plan.md` folded the execution
    //! labels into the tool definitions, which removed one of the three
    //! registries outright. The third — the `match params.name.as_str()` arms
    //! in [`dispatch_tools_call`] — cannot be folded the same way: each handler
    //! takes a different dependency set (indexer, sessions, CRDT backend, ACL
    //! caller, role), and forcing them behind one signature would add more code
    //! than it removed and obscure exactly the wiring a reader needs to see.
    //!
    //! So the arms stay a `match`, and this test makes the drift mechanical
    //! instead of hoped-for. It reads this file's own source, which sounds
    //! grubby but is the only way to enumerate match arms without introducing a
    //! *fourth* hand-maintained list — the very thing R2 is about.
    //!
    //! `tests/tool_registry_conformance.rs` covers the same invariant from
    //! outside, over the wire, but against a hand-written list of 8 names. This
    //! covers all of them and needs no maintenance. Both are worth having: that
    //! one proves the tools really answer, this one proves none was forgotten.

    use std::collections::BTreeSet;

    /// JSON-RPC methods that are dispatched by name but are not tools, so they
    /// are correctly absent from `tools/list`.
    const NON_TOOL_METHODS: &[&str] = &["initialize", "ping"];

    /// Tool names appearing as `"name" =>` arms inside `dispatch_tools_call`.
    fn dispatch_arm_names() -> BTreeSet<String> {
        let src = include_str!("mcp.rs");
        // Scope to the dispatch function so an unrelated string match elsewhere
        // in this file cannot masquerade as a tool.
        let start = src
            .find("async fn dispatch_tools_call")
            .expect("dispatch_tools_call exists");
        let body = &src[start..];
        // Brace-depth scan from the function's opening `{` rather than
        // searching for a column-0 `}`. The shortcut works under rustfmt but
        // is formatting-sensitive: a raw string or a macro body containing an
        // unindented `}` would truncate the window early and quietly shrink
        // what this test covers (codex review).
        let open = body.find('{').expect("function body");
        let mut depth = 0usize;
        let mut end = body.len();
        for (i, c) in body.char_indices().skip(open) {
            match c {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        end = i;
                        break;
                    }
                }
                _ => {}
            }
        }

        body[..end]
            .lines()
            .filter_map(|line| {
                let t = line.trim();
                let rest = t.strip_prefix('"')?;
                let (name, tail) = rest.split_once('"')?;
                tail.trim_start().starts_with("=>").then(|| name.to_owned())
            })
            .filter(|n| {
                !n.is_empty()
                    && n.chars()
                        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
            })
            .collect()
    }

    fn advertised_names() -> BTreeSet<String> {
        super::schema::tools_list_payload()["tools"]
            .as_array()
            .expect("tools array")
            .iter()
            .map(|t| t["name"].as_str().unwrap_or_default().to_owned())
            .collect()
    }

    #[test]
    fn the_parser_finds_the_arms_it_claims_to() {
        // Guards the test itself: a refactor that renames the function or
        // reshapes the arms would otherwise silently reduce this to a
        // comparison of two empty sets, which passes and proves nothing.
        let arms = dispatch_arm_names();
        assert!(
            arms.len() > 50,
            "expected the dispatch match to yield most of the tool surface, \
             found {}: the source parser has stopped working, not the registry",
            arms.len()
        );
        for m in NON_TOOL_METHODS {
            assert!(
                !arms.contains(*m),
                "`{m}` is dispatched outside dispatch_tools_call and must not \
                 appear here; the scoping window is wrong"
            );
        }
    }

    #[test]
    fn every_dispatchable_tool_is_advertised() {
        let missing: Vec<String> = dispatch_arm_names()
            .difference(&advertised_names())
            .filter(|n| !NON_TOOL_METHODS.contains(&n.as_str()))
            .cloned()
            .collect();
        assert!(
            missing.is_empty(),
            "callable but invisible to `tools/list`: {missing:?}\n\
             A client that has not read the source cannot discover these. This \
             is the direction that actually bit once, when a merge kept a \
             `purge_page` dispatch arm and dropped its schema entry."
        );
    }

    /// A tool's audience is declared once, and dispatch reads that declaration.
    ///
    /// This replaces a ratchet that parsed the source of `dispatch_tools_call`
    /// to check that the arms calling `require_admin` were exactly the tools
    /// advertising `scope: "admin"`. That test existed because the fact was
    /// written in two places; it is gone because the fact is now written in
    /// one. The gate consults `admin_scope_tools()`, which is derived from the
    /// same `Scope` the entry advertises, so the two cannot disagree.
    ///
    /// What is worth pinning is that the derived set is not empty or
    /// inverted — a bug there would silently open or close the whole operator
    /// surface. The wiring itself is covered behaviourally by
    /// `error_data::admin_required_carries_data_code`, which drives a real
    /// agent token at a real admin tool.
    #[test]
    fn admin_scope_is_declared_once_and_drives_the_gate() {
        let admin = super::schema::admin_scope_tools();
        assert!(
            admin.len() > 20,
            "admin set collapsed to {} — the operator surface would be open",
            admin.len()
        );
        for t in [
            "tenant_create",
            "rebuild",
            "register_credential",
            "import_pack",
        ] {
            assert!(
                admin.contains(t),
                "`{t}` is an operator tool but is not gated"
            );
        }
        for t in ["search", "expand", "update_page", "create_draft"] {
            assert!(
                !admin.contains(t),
                "`{t}` is agent surface but is admin-gated"
            );
        }
    }

    /// Every tool that can write must be refused on a reader replica.
    ///
    /// `READ_ONLY_REPLICA_TOOLS` is hand-kept, and its own doc comment states
    /// the cost of an omission: the write "is either silently discarded on the
    /// next `RefreshTask` hot-swap or, worse, never reaches the writer". Three
    /// tools were missing. Each reaches a page write:
    ///
    /// - `start_operation` calls `update_page_as` to stamp the run board;
    /// - `promote_changeset` calls `tool_promote_draft` per member, which
    ///   calls `tool_update_page` — and its single-draft sibling
    ///   `promote_draft` IS listed, with a comment explaining exactly why;
    /// - `discard_changeset` closes its members in the shared drafts table.
    ///
    /// The changeset pair is the worse half: the drafts are marked decided in
    /// the SHARED table, so the writer never re-offers them, while the page
    /// writes are discarded at the next hot-swap. An approved changeset then
    /// reports success and lands nowhere, with no way to re-promote it.
    ///
    /// Every tool that writes the local index is refused on a reader replica.
    ///
    /// This used to require remembering a name into `READ_ONLY_REPLICA_TOOLS`,
    /// and three tools were missing: `start_operation`, `promote_changeset`
    /// and `discard_changeset`. The list is now derived from each tool's own
    /// `Touches` declaration, so the only way to be absent from it is to
    /// declare that you do not write — a visible claim at the definition site
    /// rather than a silent omission in a list two files away.
    ///
    /// What is left to pin is the derivation itself: that the gate consults
    /// the registry, and that the tools we know write are in it.
    #[test]
    fn every_writing_tool_is_refused_on_a_reader_replica() {
        let gated = super::schema::writes_index_tools();
        for t in [
            "update_page",
            "delete_page",
            "promote_draft",
            "start_operation",
            "promote_changeset",
            "discard_changeset",
        ] {
            assert!(gated.contains(t), "`{t}` writes but is not reader-gated");
        }
        for t in ["search", "expand", "list_skills", "list_drafts"] {
            assert!(!gated.contains(t), "`{t}` only reads but is reader-gated");
        }
    }

    /// A tool that touches a shared surface is declared once, at the tool.
    ///
    /// The four surface lists were hand-kept and carried READS as well as
    /// writes — `list_drafts` is as gated as `create_draft`, because without
    /// the attach even the read hits a table that is not there. That is why
    /// the axis is separate from `writes_index` rather than folded into it,
    /// and why three tools had to appear in two lists at once.
    #[test]
    fn shared_surface_membership_is_declared_at_the_tool() {
        use super::schema::{Surface, surface_tools};
        let drafts = surface_tools(Surface::Drafts);
        assert!(drafts.contains(&"create_draft"), "write member missing");
        assert!(drafts.contains(&"list_drafts"), "READ member missing");
        assert!(
            surface_tools(Surface::Events).contains(&"capture_event"),
            "events member missing"
        );
        // The overlap case: writes the index AND touches a shared surface.
        let t = super::schema::touches_of("promote_draft").expect("declared");
        assert!(t.writes_index && t.surface == Some(Surface::Drafts));
    }

    #[test]
    fn every_advertised_tool_has_a_dispatch_arm() {
        let unroutable: Vec<String> = advertised_names()
            .difference(&dispatch_arm_names())
            .cloned()
            .collect();
        assert!(
            unroutable.is_empty(),
            "advertised by `tools/list` with no dispatch arm: {unroutable:?}"
        );
    }
}
