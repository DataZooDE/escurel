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
    SkillCapabilities as TypesSkillCapabilities, TenantCreateResponse, TenantDeleteResponse,
    TenantGetResponse, TenantImportResponse, TenantListResponse, TenantSpec as TypesTenantSpec,
    TenantUpdateResponse, WebhookDeliveriesResponse, WebhookDelivery,
};
use serde::Deserialize;
use serde_json::{Value, json};
use tracing::Instrument;

mod ingest;
mod schema;
mod tools_admin;
pub(crate) use ingest::{ingest, ingest_upload};
pub(crate) use schema::openapi_document;
use schema::{page_type_str, tools_list_payload};
use tools_admin::*;

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
        Some(verifier) => {
            let served = state.served_tenant.as_deref();
            match crate::auth_gate::enforce_auth(verifier, &headers, served).await {
                Ok(ctx) => Some(ctx),
                Err(resp) => return resp,
            }
        }
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
    let admin_value = state
        .verifier
        .as_ref()
        .map(|v| v.config().admin_role_value.clone());
    let token_groups: Vec<String> = auth_ctx
        .as_ref()
        .map(|c| {
            c.groups
                .iter()
                .filter(|g| Some(g.as_str()) != admin_value.as_deref())
                .cloned()
                .collect()
        })
        .unwrap_or_default();

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
                req.params,
            )
            .await
            .map(wrap_tool_result);
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
    // would read back one-less-than-full.
    if name.starts_with("admin_")
        || name.starts_with("tenant_")
        || matches!(
            name,
            "rebuild"
                | "compact_lanes"
                | "attach_external"
                | "embedding_reload"
                | "publish_snapshot"
        )
    {
        return None;
    }
    Some(match name {
        // `apply_op` is a write; `open_session` debits a session
        // slot (semaphore, not a token bucket) inside the tool
        // body; `close_session` is a cleanup and does not debit.
        "update_page" | "delete_page" | "move_page" | "apply_op" | "append_message"
        | "capture_event" | "assign_event" => Dimension::Writes,
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
/// Deserialize a tool's arguments, reporting a parse failure as
/// `invalid_params` tagged with the tool name.
///
/// Every tool handler opened with the same two-line incantation; naming it
/// once means a handler's first line is about the tool rather than about
/// serde. See `docs/notes/complexity-reduction-plan.md` R4.
fn parse_args<T: serde::de::DeserializeOwned>(
    args: Value,
    tool: &str,
) -> Result<T, JsonRpcError> {
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
///   "isError": false
/// }
/// ```
///
/// `content[0].text` is the payload serialised to a JSON string — that
/// is what a text-only MCP client (Claude Code) reads. `structuredContent`
/// carries the raw payload object for programmatic clients (escurel-client
/// decodes this). Applied to the SUCCESS value of `tools/call` ONLY; tool
/// errors keep the JSON-RPC error envelope, and `initialize` / `ping` /
/// `tools/list` are returned raw (they are not `CallToolResult`s).
fn wrap_tool_result(payload: Value) -> Value {
    let text = serde_json::to_string(&payload).unwrap_or_else(|_| payload.to_string());
    json!({
        "content": [ { "type": "text", "text": text } ],
        "structuredContent": payload,
        "isError": false,
    })
}

/// Mutating tool surface a ducklake reader must reject (DuckLake PR 6):
/// each of these writes into the SERVING index, which on a reader is a
/// throwaway in-memory copy adopted from the lake (`adopt_lake`) — any
/// write here is either silently discarded on the next `RefreshTask`
/// hot-swap or, worse, never reaches the writer/lake at all. The writer
/// is the only mutation path; a reader is read-only by construction.
const READ_ONLY_REPLICA_TOOLS: &[&str] = &[
    "update_page",
    "delete_page",
    "move_page",
    "rebuild",
    "compact_lanes",
    "import_pack",
    "rebase_pack",
    "unsubscribe_pack",
    "submit_promotion",
    "attach_external",
    "add_group_member",
    "remove_group_member",
    "register_credential",
    "delete_credential",
    // A reader has no local mutation surface to publish FROM (it only
    // ever adopts). "retry against the writer" is the exact correct
    // guidance here, so `publish_snapshot` reuses this bucket rather
    // than a bespoke reader-side error (DuckLake PR 7).
    "publish_snapshot",
    "create_sql_instance",
    "register_endpoint",
    "delete_endpoint",
    "create_remote_instance",
    "tenant_create",
    "tenant_update",
    "tenant_delete",
    "tenant_import",
];

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

/// The chat tool surface `dispatch_tools_call`'s dynamic reader gate
/// covers — split out from [`UNSUPPORTED_ON_REPLICA_TOOLS`] (and, for
/// `admin_delete_chat_history`, out of [`READ_ONLY_REPLICA_TOOLS`])
/// because whether they're servable depends on the CURRENT indexer's
/// chat backend, not just `state.reader_mode` (DuckLake PR 8). The GDPR
/// delete path deliberately gets the same treatment as append/list: on
/// the shared attached-Postgres table a delete from ANY replica removes
/// the rows for every replica (same physical table), so there is no
/// reason to force it through the writer once chat is re-homed.
const CHAT_TOOLS: &[&str] = &[
    "append_message",
    "list_messages",
    "admin_delete_chat_history",
];

/// The events tool surface `dispatch_tools_call`'s dynamic reader gate
/// covers (DuckLake PR 9, Phase B) — mirrors [`CHAT_TOOLS`] exactly:
/// reader-rejected only when the CURRENT indexer has no shared events
/// backend attached (see [`escurel_index::Indexer::has_shared_events`]).
const EVENTS_TOOLS: &[&str] = &["capture_event", "assign_event", "list_events", "list_inbox"];

/// The CRDT/session tool surface `dispatch_tools_call`'s dynamic reader
/// gate covers (DuckLake PR 10, Phase B) — mirrors [`CHAT_TOOLS`] /
/// [`EVENTS_TOOLS`] exactly: reader-rejected only when the CURRENT
/// indexer has no shared CRDT backend attached (see
/// [`escurel_index::Indexer::has_shared_crdt`]).
///
/// Gated on the INDEXER's `has_shared_crdt` for all four tools, including
/// the three session ones (`open_session`/`apply_op`/`close_session`,
/// which route through `state.crdt_backend`, not the indexer) — both
/// seams are attached from the SAME `catalog_dsn` at the SAME boot step
/// (`EscurelConfig::build`'s `is_pg_catalog()` branch), so they always
/// agree; checking the indexer keeps this list's shape identical to
/// [`CHAT_TOOLS`]/[`EVENTS_TOOLS`] rather than inventing a second style
/// of dynamic check just for these three tools.
///
/// Scope note: this makes the durable STORAGE (`crdt_ops`/
/// `crdt_snapshots`) reachable from every replica — it does NOT give a
/// live editing session cross-replica failover. `SessionManager` still
/// runs one `LiveDoc` actor per page in-process; a session opened on
/// replica A and continued on replica B is not the same actor. What this
/// buys a reader: `list_snapshots` works for any page (even one whose
/// history was written by the writer or another reader), and a session
/// opened fresh on any replica loads correct history from the shared
/// table. Ingress affinity for a live session is a documented future
/// follow-up, not built here.
const CRDT_TOOLS: &[&str] = &[
    "open_session",
    "apply_op",
    "close_session",
    "list_snapshots",
];

async fn dispatch_tools_call(
    state: &crate::server::AppState,
    tenant_id: &str,
    role: Option<Role>,
    subject: &str,
    token_groups: &[String],
    params: Value,
) -> Result<Value, JsonRpcError> {
    let params: ToolsCallParams = serde_json::from_value(params)
        .map_err(|e| JsonRpcError::invalid_params(format!("tools/call params: {e}")))?;

    // Ducklake-reader gate (DuckLake PR 6): reject the mutating tool
    // surface and the chat/CRDT/session/event tool surface EARLY —
    // before any tool-specific handler runs, before the indexer/session
    // routing below — with a typed error naming which bucket applies.
    if state.reader_mode {
        if READ_ONLY_REPLICA_TOOLS.contains(&params.name.as_str()) {
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

    // Dynamic chat gate (DuckLake PR 8): a reader rejects
    // `append_message`/`list_messages` UNLESS the current indexer has a
    // shared chat backend attached (`ESCUREL_INDEX_BACKEND=ducklake` with
    // a Postgres catalog — see `EscurelConfig::build`). Checked against
    // the SAME captured indexer the rest of this call runs against, so a
    // hot-swap mid-flight can't disagree with itself. Every non-reader
    // deployment (single-file, or a ducklake writer) is completely
    // unaffected — this block is inert there.
    if state.reader_mode
        && CHAT_TOOLS.contains(&params.name.as_str())
        && !current_indexer
            .as_deref()
            .is_some_and(Indexer::has_shared_chat)
    {
        return Err(JsonRpcError::unsupported_on_replica(params.name.clone()));
    }

    // Dynamic events gate (DuckLake PR 9): mirrors the chat gate above
    // exactly — a reader rejects `capture_event`/`assign_event`/
    // `list_events`/`list_inbox` UNLESS the current indexer has a shared
    // events backend attached.
    if state.reader_mode
        && EVENTS_TOOLS.contains(&params.name.as_str())
        && !current_indexer
            .as_deref()
            .is_some_and(Indexer::has_shared_events)
    {
        return Err(JsonRpcError::unsupported_on_replica(params.name.clone()));
    }

    // Dynamic CRDT gate (DuckLake PR 10): mirrors the chat/events gates
    // above exactly — a reader rejects `open_session`/`apply_op`/
    // `close_session`/`list_snapshots` UNLESS the current indexer has a
    // shared CRDT backend attached.
    if state.reader_mode
        && CRDT_TOOLS.contains(&params.name.as_str())
        && !current_indexer
            .as_deref()
            .is_some_and(Indexer::has_shared_crdt)
    {
        return Err(JsonRpcError::unsupported_on_replica(params.name.clone()));
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
        "export_pack" => {
            require_admin(role)?;
            return tool_export_pack(state, params.arguments).await;
        }
        "import_pack" => {
            require_admin(role)?;
            return tool_import_pack(state, params.arguments).await;
        }
        "list_packs" => {
            require_admin(role)?;
            return tool_list_packs(state).await;
        }
        "rebase_pack" => {
            require_admin(role)?;
            return tool_rebase_pack(state, params.arguments).await;
        }
        "unsubscribe_pack" => {
            require_admin(role)?;
            return tool_unsubscribe_pack(state, params.arguments).await;
        }
        "submit_promotion" => {
            require_admin(role)?;
            return tool_submit_promotion(state, subject, params.arguments).await;
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
        "publish_snapshot" => {
            require_admin(role)?;
            return tool_publish_snapshot(state).await;
        }
        // Outbound-webhook delivery log (observability). Needs only the
        // webhook handle on AppState, so it routes before the indexer gate.
        "admin_webhook_deliveries" => {
            require_admin(role)?;
            return tool_admin_webhook_deliveries(state, params.arguments);
        }
        _ => {}
    }

    let indexer = current_indexer.as_ref().ok_or_else(|| {
        JsonRpcError::internal("server has no indexer wired; tools/call is unavailable")
    })?;

    // Deterministic per-instance ACL caller (escurel-index). The admin
    // role bypasses owner-visibility; a missing role is dev/on-host mode
    // (no verifier, open gateway) and likewise bypasses — there is no
    // subject to scope against. A real Agent token is enforced.
    // `token_groups` are the RBAC groups from the JWT (admin-value already
    // stripped by the caller in `mcp_inner`).
    let caller = AclCaller {
        subject,
        is_admin: matches!(role, None | Some(Role::Admin)),
        token_groups,
    };

    match params.name.as_str() {
        "list_skills" => tool_list_skills(indexer).await,
        "list_instances" => tool_list_instances(indexer, caller, params.arguments).await,
        "resolve" => tool_resolve(indexer, caller, params.arguments).await,
        "expand" => tool_expand(state, indexer, caller, params.arguments).await,
        "fetch_blob" => tool_fetch_blob(indexer, caller, params.arguments).await,
        "neighbours" => tool_neighbours(indexer, caller, params.arguments).await,
        "provenance_ancestry" => tool_provenance_ancestry(indexer, caller, params.arguments).await,
        "expectation_drift" => tool_expectation_drift(indexer, caller, params.arguments).await,
        "abandoned_paths" => tool_abandoned_paths(indexer, caller, params.arguments).await,
        "provenance_path" => tool_provenance_path(indexer, caller, params.arguments).await,
        "search" => tool_search(indexer, caller, params.arguments).await,
        "run_stored_query" => {
            // A stored query runs pre-declared arbitrary SQL over the whole
            // corpus and returns arbitrary projected columns (aggregates,
            // joins) — there is no per-row owner to filter on, so the ACL is
            // at the capability level: operator/analytics only.
            require_admin(role)?;
            tool_run_stored_query(indexer, params.arguments).await
        }
        // A parameterized read over ONE sql_view instance's view. Unlike
        // run_stored_query this is an agent-surface tool: the per-instance
        // ACL gates the target instance (the data), so it is not admin-gated
        // (issue #205).
        "query_instance" => tool_query_instance(indexer, caller, params.arguments).await,
        "validate" => tool_validate(indexer, params.arguments).await,
        "update_page" => {
            tool_update_page(state, indexer, caller, state.write_acl, params.arguments).await
        }
        "delete_page" => {
            tool_delete_page(state, indexer, caller, state.write_acl, params.arguments).await
        }
        "move_page" => {
            tool_move_page(state, indexer, caller, state.write_acl, params.arguments).await
        }
        "append_message" => {
            tool_append_message(indexer, caller, state.write_acl, params.arguments).await
        }
        "list_messages" => {
            tool_list_messages(indexer, caller, state.write_acl, params.arguments).await
        }
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
            tool_admin_quota(state, tenant_id, params.arguments)
        }
        "admin_audit" => {
            require_admin(role)?;
            tool_admin_audit(indexer, params.arguments).await
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
        // Group ACL v1: admin-only membership mutation + read (D14). Gated
        // here, exactly like the other operator tools; group membership is
        // the source of truth for custom-group RBAC.
        "add_group_member" => {
            require_admin(role)?;
            tool_add_group_member(indexer, subject, params.arguments).await
        }
        "remove_group_member" => {
            require_admin(role)?;
            tool_remove_group_member(indexer, params.arguments).await
        }
        "list_group_members" => {
            require_admin(role)?;
            tool_list_group_members(indexer, params.arguments).await
        }
        // SQL-view credential registry (admin-only). Secrets live
        // server-side in kb.duckdb, never in the markdown corpus (REQ-SQL-05).
        "register_credential" => {
            require_admin(role)?;
            tool_register_credential(indexer, subject, params.arguments).await
        }
        "list_credentials" => {
            require_admin(role)?;
            tool_list_credentials(indexer).await
        }
        "delete_credential" => {
            require_admin(role)?;
            tool_delete_credential(indexer, params.arguments).await
        }
        "validate_bindings" => {
            require_admin(role)?;
            tool_validate_bindings(indexer).await
        }
        "create_sql_instance" => {
            require_admin(role)?;
            tool_create_sql_instance(indexer, params.arguments).await
        }
        // Remote-backend endpoint registry (admin-only). Base URL + auth live
        // server-side in kb.duckdb; the secret is never echoed. This is the
        // SSRF guard — a remote instance can only reach a registered endpoint.
        "register_endpoint" => {
            require_admin(role)?;
            tool_register_endpoint(indexer, subject, params.arguments).await
        }
        "list_endpoints" => {
            require_admin(role)?;
            tool_list_endpoints(indexer).await
        }
        "delete_endpoint" => {
            require_admin(role)?;
            tool_delete_endpoint(indexer, params.arguments).await
        }
        "validate_endpoints" => {
            require_admin(role)?;
            tool_validate_endpoints(indexer).await
        }
        // Materialise a remote (openapi/mcp) overlay page from a skill that
        // declares a remote backend. Admin-only, mirroring create_sql_instance.
        "create_remote_instance" => {
            require_admin(role)?;
            tool_create_remote_instance(indexer, params.arguments).await
        }
        // Write-back to a remote instance's upstream. Agent tool, gated by the
        // target instance's acl.update (may_write_instance, fail-closed).
        "write_instance" => tool_write_instance(indexer, caller, params.arguments).await,
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
                visibility: match s.visibility {
                    Visibility::Public => "public".to_string(),
                    Visibility::Owner => "owner".to_string(),
                },
                owner_field: s.owner_field,
                acl: s.acl.map(|a| TypesSkillAcl {
                    read: a.read,
                    create: a.create,
                    update: a.update,
                    delete: a.delete,
                }),
                backend: TypesSkillBackend {
                    kind: s.backend.kind.as_str().to_string(),
                },
                capabilities: {
                    let c = Capabilities::for_kind(s.backend.kind);
                    TypesSkillCapabilities {
                        writable: c.writable,
                        granularity: c.granularity.as_str().to_string(),
                        search: c.search.as_str().to_string(),
                        supports_crdt: c.supports_crdt,
                    }
                },
                layer: s.layer.unwrap_or_else(|| "overlay".to_owned()),
                shadows: s.shadows,
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

async fn tool_list_instances(
    indexer: &Indexer,
    caller: AclCaller<'_>,
    args: Value,
) -> Result<Value, JsonRpcError> {
    let a: ListInstancesArgs = parse_args(args, "list_instances")?;
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
    // Deterministic ACL filter: drop owner-private instances the caller
    // does not own (admin bypasses). Enumeration must not leak what a
    // direct read would deny.
    let mut instances = Vec::with_capacity(out.len());
    for i in &out {
        if indexer
            .may_read_instance(&caller, &i.skill, &i.frontmatter)
            .await
            .map_err(|e| JsonRpcError::internal(format!("list_instances acl: {e}")))?
        {
            instances.push(json!({
                "page_id": i.page_id,
                "skill": i.skill,
                "frontmatter": i.frontmatter,
                "at": i.at,
            }));
        }
    }
    Ok(json!({
        "instances": instances,
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

async fn tool_resolve(
    indexer: &Indexer,
    caller: AclCaller<'_>,
    args: Value,
) -> Result<Value, JsonRpcError> {
    let a: ResolveArgs = parse_args(args, "resolve")?;
    let mut resolved = indexer
        .resolve(&a.wikilink, a.scenario.as_deref())
        .await
        .map_err(|e| JsonRpcError::internal(format!("resolve: {e}")))?;
    // ACL (always on, mirroring the read filters): never disclose the
    // existence / page_id of an owner-private instance the caller cannot
    // read — resolve it to "not found", exactly as `expand` returns null.
    if let Some(p) = &resolved.page
        && p.page_type == PageType::Instance
    {
        let readable = match indexer
            .expand(&p.page_id, None, None)
            .await
            .map_err(|e| JsonRpcError::internal(format!("resolve acl: {e}")))?
        {
            Some(e) => indexer
                .may_read_instance(&caller, &p.skill, &e.frontmatter)
                .await
                .map_err(|e| JsonRpcError::internal(format!("resolve acl: {e}")))?,
            None => true,
        };
        if !readable {
            resolved.page = None;
        }
    }
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
    /// Return ALL chunks of a document instance instead of the bounded lead
    /// (REQ-DOC-05). For a single-document detail view (relevance heatmap over
    /// the whole text), not the default grounding/preview path.
    #[serde(default)]
    full: bool,
}

async fn tool_expand(
    state: &crate::server::AppState,
    indexer: &Indexer,
    caller: AclCaller<'_>,
    args: Value,
) -> Result<Value, JsonRpcError> {
    let a: ExpandArgs = parse_args(args, "expand")?;
    let out = indexer
        .expand(&a.page_id, a.as_of.as_deref(), a.scenario.as_deref())
        .await
        .map_err(|e| JsonRpcError::internal(format!("expand: {e}")))?;
    // Deterministic ACL: an owner-private instance the caller does not own
    // reads as absent (null) — same shape as a missing page, so existence
    // is not leaked. Skill pages are the public catalogue, never gated.
    if let Some(e) = &out
        && e.page.page_type == PageType::Instance
        && !indexer
            .may_read_instance(&caller, &e.page.skill, &e.frontmatter)
            .await
            .map_err(|err| JsonRpcError::internal(format!("expand acl: {err}")))?
    {
        return Ok(json!({ "page": Value::Null }));
    }
    match out {
        None => Ok(json!({ "page": Value::Null })),
        Some(e) => {
            let e_page_id = e.page.page_id.clone();
            let mut page = json!({
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
            });
            // #246: surface the page's current monotonic version so a client
            // can pass it back as `base_version` on the next `update_page`
            // (the read→edit→write optimistic-concurrency cycle).
            if let Some(backend) = state.crdt_backend.as_ref() {
                let hlc =
                    u64::try_from(backend.max_hlc(&e_page_id).await.unwrap_or(0)).unwrap_or(0);
                page["version"] = json!(Version::from_op_count(hlc).as_str());
            }
            // Shadowed base (REQ-LAYER-03): when a tenant OVERLAY skill page
            // shadows a pack-imported base skill of the same slug, expose the
            // base page + its frontmatter under a namespaced `shadow` object —
            // the same drift-visibility discipline as the sql_view `source`
            // namespace: the overlay wins for display, the base value stays
            // visible, never silently masked.
            if e.page.page_type == PageType::Skill
                && !e_page_id.starts_with(escurel_index::pack::RESERVED_BASE_PREFIX)
                && let Some(slug) = e.page.slug.as_deref()
                && let Some((base_page_id, pin, base_fm)) = indexer
                    .shadowed_base(slug, &e_page_id)
                    .await
                    .map_err(|err| JsonRpcError::internal(format!("expand shadow: {err}")))?
            {
                page["shadow"] = json!({
                    "base_page_id": base_page_id,
                    "pack": pin,
                    "base": base_fm,
                });
            }
            // SQL-view overlay: render a BOUNDED projection beneath the overlay
            // body (REQ-SQL-06), and expose projected source columns under a
            // namespaced `source` object so overlay↔source drift is visible
            // without the overlay value being masked (REQ-OV-02). The overlay
            // (shown first) always wins for display.
            if let Some(proj) = sql_view_projection(indexer, &e).await {
                page["backend_projection"] = proj;
            }
            // Document overlay: bound the chunks returned (REQ-DOC-05) — never
            // the full document text. With no query in `expand`, return the
            // lead (first K chunks) and flag truncation.
            if e.frontmatter
                .get("backend_ref")
                .and_then(|b| b.get("kind"))
                .and_then(Value::as_str)
                == Some("document")
            {
                // The skill's `lead_chunks` caps the lead returned (REQ-DOC-05);
                // fall back to the server default. The full text lives in the blob.
                const DEFAULT_CHUNK_LEAD: usize = 8;
                let lead_n = indexer
                    .skill_backend(&e.page.skill)
                    .await
                    .ok()
                    .and_then(|b| b.document.and_then(|d| d.lead_chunks))
                    .unwrap_or(DEFAULT_CHUNK_LEAD);
                let total = e.blocks.len();
                // `full` returns every chunk (detail/heatmap view); otherwise
                // bound to the lead (REQ-DOC-05).
                if !a.full
                    && let Some(arr) = page["blocks"].as_array().cloned()
                {
                    let lead: Vec<Value> = arr.into_iter().take(lead_n).collect();
                    page["blocks"] = Value::from(lead);
                }
                page["chunks_total"] = json!(total);
                page["chunks_truncated"] = json!(!a.full && total > lead_n);
            }
            // Remote (proxy) overlay: fetch the LIVE projection from the
            // upstream openapi/mcp endpoint (nothing is materialised in
            // DuckDB). A failure resolves to `{ issue }` — the overlay page is
            // still returned, never a partial/fabricated body.
            if e.frontmatter
                .get("backend_ref")
                .and_then(|b| b.get("kind"))
                .and_then(Value::as_str)
                .is_some_and(|k| k == "openapi" || k == "mcp")
            {
                page["backend_projection"] = crate::remote_backend::fetch_projection(
                    indexer,
                    &e.page.skill,
                    e.page.slug.as_deref(),
                )
                .await;
            }
            Ok(page)
        }
    }
}

/// Hard cap on a single `fetch_blob` transfer (25 MiB). Original documents are
/// larger than the admin lane cap but still bounded for a browser preview.
const FETCH_BLOB_MAX_BYTES: usize = 25 * 1024 * 1024;

#[derive(Deserialize)]
struct FetchBlobArgs {
    page_id: String,
}

/// Return the ORIGINAL retained file bytes for a `document`-backed instance —
/// the blob behind `backend_ref.blob_id` — base64-encoded with a sniffed
/// content type, for a faithful client-side preview of the source document.
///
/// ACL mirrors `expand`: an instance the caller may not read resolves to a null
/// blob (existence is not leaked). Non-document pages and missing pages also
/// resolve to null. The transfer is size-capped.
async fn tool_fetch_blob(
    indexer: &Indexer,
    caller: AclCaller<'_>,
    args: Value,
) -> Result<Value, JsonRpcError> {
    let a: FetchBlobArgs = parse_args(args, "fetch_blob")?;
    let out = indexer
        .expand(&a.page_id, None, None)
        .await
        .map_err(|e| JsonRpcError::internal(format!("fetch_blob expand: {e}")))?;
    let Some(e) = out else {
        return Ok(json!({ "blob": Value::Null }));
    };
    if e.page.page_type == PageType::Instance
        && !indexer
            .may_read_instance(&caller, &e.page.skill, &e.frontmatter)
            .await
            .map_err(|err| JsonRpcError::internal(format!("fetch_blob acl: {err}")))?
    {
        return Ok(json!({ "blob": Value::Null }));
    }
    let backend_ref = e.frontmatter.get("backend_ref");
    let is_doc = backend_ref
        .and_then(|b| b.get("kind"))
        .and_then(Value::as_str)
        == Some("document");
    let blob_id_str = backend_ref
        .and_then(|b| b.get("blob_id"))
        .and_then(Value::as_str);
    if !is_doc || blob_id_str.is_none() {
        return Ok(json!({ "blob": Value::Null }));
    }
    let blob_id = escurel_storage::BlobId::parse(blob_id_str.unwrap())
        .ok_or_else(|| JsonRpcError::internal("fetch_blob: malformed blob_id"))?;
    let bytes = indexer
        .read_blob(&blob_id)
        .await
        .map_err(|err| JsonRpcError::internal(format!("fetch_blob read: {err}")))?;
    if bytes.len() > FETCH_BLOB_MAX_BYTES {
        return Err(JsonRpcError::invalid_params(format!(
            "blob is {} bytes, over the {FETCH_BLOB_MAX_BYTES}-byte fetch cap",
            bytes.len()
        )));
    }
    Ok(json!({
        "blob": {
            "page_id": e.page.page_id,
            "content_type": sniff_content_type(&bytes),
            "size": bytes.len(),
            "bytes_base64": B64.encode(&bytes),
        }
    }))
}

/// Best-effort content-type sniff for a retained blob: PDF, OOXML
/// (docx/pptx/xlsx by their part markers), then UTF-8 text.
fn sniff_content_type(bytes: &[u8]) -> &'static str {
    fn contains(hay: &[u8], needle: &[u8]) -> bool {
        needle.len() <= hay.len() && hay.windows(needle.len()).any(|w| w == needle)
    }
    if bytes.starts_with(b"%PDF") {
        return "application/pdf";
    }
    if bytes.starts_with(b"PK\x03\x04") {
        if contains(bytes, b"word/document.xml") {
            return "application/vnd.openxmlformats-officedocument.wordprocessingml.document";
        }
        if contains(bytes, b"ppt/presentation.xml") {
            return "application/vnd.openxmlformats-officedocument.presentationml.presentation";
        }
        if contains(bytes, b"xl/workbook.xml") {
            return "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet";
        }
        return "application/zip";
    }
    if std::str::from_utf8(bytes).is_ok() {
        return "text/plain";
    }
    "application/octet-stream"
}

/// Bounded rows + projected `source` fields for a SQL-view instance overlay,
/// or `None` when the page is not a `sql_view` instance.
async fn sql_view_projection(indexer: &Indexer, e: &escurel_index::ExpandedPage) -> Option<Value> {
    /// Default rows rendered when the skill declares no `projection_limit`.
    const DEFAULT_PROJECTION_LIMIT: usize = 50;
    let backend_ref = e.frontmatter.get("backend_ref")?;
    if backend_ref.get("kind").and_then(Value::as_str) != Some("sql_view") {
        return None;
    }
    let view = backend_ref.get("view").and_then(Value::as_str)?;

    // Fail closed on schema drift (REQ-NF-06): if the view's current schema
    // fingerprint no longer matches the one captured at create time, return
    // an Issue instead of (possibly wrong) rows.
    if let Some(stored) = backend_ref
        .get("source_schema_fingerprint")
        .and_then(Value::as_str)
    {
        match indexer.current_view_fingerprint(view).await {
            Ok(current) if current != stored => {
                return Some(json!({
                    "view": view, "rows": [], "source": {},
                    "issue": { "code": "binding_degraded",
                        "message": "source schema drifted from the stored fingerprint; \
                                    reads fail closed until the binding is re-validated" },
                }));
            }
            Err(e) => {
                return Some(json!({
                    "view": view, "rows": [], "source": {},
                    "issue": { "code": "source_unavailable", "message": e.to_string() },
                }));
            }
            Ok(_) => {}
        }
    }

    // The skill's `projection_limit` caps the rows rendered (REQ-SQL-06); fall
    // back to the server default, and never exceed the policy cap (so the
    // `limit + 1` truncation sentinel can't be silently clamped by the row
    // reader). Fetch one extra row so `truncated` is exact.
    let binding = indexer.skill_backend(&e.page.skill).await.ok();
    let limit = binding
        .as_ref()
        .and_then(|b| b.projection_limit)
        .unwrap_or(DEFAULT_PROJECTION_LIMIT)
        .min(escurel_index::backend::MAX_PROJECTION_ROWS);
    let mut rows = indexer.project_view(view, limit + 1).await.ok()?;
    let truncated = rows.len() > limit;
    rows.truncate(limit);

    // Expose projected source columns under `source.<overlay_field>` per the
    // skill's `project` map (drift-visible; overlay wins for display).
    let mut source = serde_json::Map::new();
    if let Some(sv) = binding.and_then(|b| b.sql_view)
        && let Some(first) = rows.first()
    {
        for (src_col, overlay_field) in &sv.project {
            if let Some(v) = first.get(src_col) {
                source.insert(overlay_field.clone(), v.clone());
            }
        }
    }

    Some(json!({
        "view": view,
        "rows": rows,
        "source": source,
        "truncated": truncated,
    }))
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

async fn tool_neighbours(
    indexer: &Indexer,
    caller: AclCaller<'_>,
    args: Value,
) -> Result<Value, JsonRpcError> {
    let a: NeighboursArgs = parse_args(args, "neighbours")?;
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
    // ACL (always on): drop edges whose OTHER endpoint is an owner-private
    // instance the caller can't read — don't reveal links to/from private
    // records. The queried page itself is the caller's vantage point.
    let mut out = Vec::with_capacity(edges.len());
    for e in &edges {
        let neighbour = if e.src_page == a.page_id {
            &e.dst_page
        } else {
            &e.src_page
        };
        let readable = match indexer
            .expand(neighbour, None, None)
            .await
            .map_err(|err| JsonRpcError::internal(format!("neighbours acl: {err}")))?
        {
            Some(ex) if ex.page.page_type == PageType::Instance => indexer
                .may_read_instance(&caller, &ex.page.skill, &ex.frontmatter)
                .await
                .map_err(|err| JsonRpcError::internal(format!("neighbours acl: {err}")))?,
            _ => true, // non-instance / absent → not owner-gated
        };
        if readable {
            out.push(json!({
                "src_page": e.src_page,
                "dst_page": e.dst_page,
                "link_skill": e.link_skill,
                "link_version": e.link_version,
                "dst_anchor": e.dst_anchor,
            }));
        }
    }
    Ok(json!({ "edges": out }))
}

/// Default hop depth when the caller omits `max_hops`.
const PROVENANCE_DEFAULT_HOPS: u32 = 5;

#[derive(Deserialize)]
struct ProvenanceAncestryArgs {
    page_id: String,
    /// `up` (everything this rests on) | `down` (everything derived from it).
    #[serde(default)]
    direction: Option<String>,
    /// Restrict the walk to these edge kinds; empty/absent = all.
    #[serde(default)]
    relations: Option<Vec<String>>,
    /// Hop ceiling; defaults to `PROVENANCE_DEFAULT_HOPS`, clamped server-side.
    #[serde(default)]
    max_hops: Option<u32>,
    /// RFC 3339 time-travel cut; edges from sources born after it are hidden.
    #[serde(default)]
    as_of: Option<String>,
}

async fn tool_provenance_ancestry(
    indexer: &Indexer,
    caller: AclCaller<'_>,
    args: Value,
) -> Result<Value, JsonRpcError> {
    let a: ProvenanceAncestryArgs = parse_args(args, "provenance_ancestry")?;
    let dir = match a.direction.as_deref().unwrap_or("up") {
        "up" => GraphDir::Up,
        "down" => GraphDir::Down,
        other => {
            return Err(JsonRpcError::invalid_params(format!(
                "provenance_ancestry direction `{other}`; expected up|down"
            )));
        }
    };
    let relations = a.relations.unwrap_or_default();
    let rel_opt = (!relations.is_empty()).then_some(relations.as_slice());
    let hops = indexer
        .provenance_ancestry(
            &a.page_id,
            dir,
            rel_opt,
            a.max_hops.unwrap_or(PROVENANCE_DEFAULT_HOPS),
            a.as_of.as_deref(),
        )
        .await
        .map_err(|e| JsonRpcError::internal(format!("provenance_ancestry: {e}")))?;

    // ACL (always on): resolve the readability of every page touched — the
    // reached nodes AND the interior nodes of each path — then drop any hop
    // whose path crosses an owner-private instance the caller can't read
    // (fail-closed transitive visibility). The start page (path[0]) is the
    // caller's own vantage point and is not gated, mirroring `neighbours`.
    let mut readable: std::collections::HashMap<String, bool> = std::collections::HashMap::new();
    for h in &hops {
        for pid in h.path.iter().skip(1) {
            if !readable.contains_key(pid) {
                let r =
                    match indexer.expand(pid, None, None).await.map_err(|e| {
                        JsonRpcError::internal(format!("provenance_ancestry acl: {e}"))
                    })? {
                        Some(ex) if ex.page.page_type == PageType::Instance => indexer
                            .may_read_instance(&caller, &ex.page.skill, &ex.frontmatter)
                            .await
                            .map_err(|e| {
                                JsonRpcError::internal(format!("provenance_ancestry acl: {e}"))
                            })?,
                        _ => true,
                    };
                readable.insert(pid.clone(), r);
            }
        }
    }
    // Emit one row per node at its shallowest depth (rows arrive ordered by
    // depth), keeping only hops whose whole path (past the vantage) is
    // readable.
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for h in &hops {
        if !h
            .path
            .iter()
            .skip(1)
            .all(|p| *readable.get(p).unwrap_or(&false))
        {
            continue;
        }
        if seen.insert(h.page_id.clone()) {
            out.push(json!({
                "page_id": h.page_id,
                "skill": h.skill,
                "relation": h.relation,
                "depth": h.depth,
            }));
        }
    }
    Ok(json!({ "hops": out }))
}

/// Whether `page_id` is readable by `caller`: a non-instance / absent page
/// is not owner-gated (true); an instance goes through `may_read_instance`
/// (fail-closed). Shared by the provenance analytics ACL filters.
async fn provenance_page_readable(
    indexer: &Indexer,
    caller: &AclCaller<'_>,
    page_id: &str,
) -> Result<bool, JsonRpcError> {
    match indexer
        .expand(page_id, None, None)
        .await
        .map_err(|e| JsonRpcError::internal(format!("provenance acl: {e}")))?
    {
        Some(ex) if ex.page.page_type == PageType::Instance => indexer
            .may_read_instance(caller, &ex.page.skill, &ex.frontmatter)
            .await
            .map_err(|e| JsonRpcError::internal(format!("provenance acl: {e}"))),
        _ => Ok(true),
    }
}

#[derive(Deserialize)]
struct ExpectationDriftArgs {
    /// Restrict to decisions of this skill; absent/empty = all.
    #[serde(default)]
    skill: Option<String>,
}

async fn tool_expectation_drift(
    indexer: &Indexer,
    caller: AclCaller<'_>,
    args: Value,
) -> Result<Value, JsonRpcError> {
    let a: ExpectationDriftArgs = parse_args(args, "expectation_drift")?;
    let skill = a.skill.filter(|s| !s.is_empty());
    let rows = indexer
        .expectation_drift(skill.as_deref())
        .await
        .map_err(|e| JsonRpcError::internal(format!("expectation_drift: {e}")))?;

    // Fail-closed: drop a row if ANY of the three pages it references is
    // unreadable — never disclose a drift edge that touches a private record.
    let mut out = Vec::new();
    for r in &rows {
        let mut visible = true;
        for pid in [
            &r.decision_page_id,
            &r.expectation_page_id,
            &r.superseding_page_id,
        ] {
            if !provenance_page_readable(indexer, &caller, pid).await? {
                visible = false;
                break;
            }
        }
        if visible {
            out.push(json!({
                "decision_page_id": r.decision_page_id,
                "decision_skill": r.decision_skill,
                "expectation_page_id": r.expectation_page_id,
                "superseding_page_id": r.superseding_page_id,
                "decided_at": r.decided_at,
                "superseded_at": r.superseded_at,
            }));
        }
    }
    Ok(json!({ "rows": out }))
}

#[derive(Deserialize)]
struct AbandonedPathsArgs {
    /// Restrict to retired nodes of this skill; absent/empty = all.
    #[serde(default)]
    skill: Option<String>,
}

async fn tool_abandoned_paths(
    indexer: &Indexer,
    caller: AclCaller<'_>,
    args: Value,
) -> Result<Value, JsonRpcError> {
    let a: AbandonedPathsArgs = parse_args(args, "abandoned_paths")?;
    let skill = a.skill.filter(|s| !s.is_empty());
    let nodes = indexer
        .abandoned_paths(skill.as_deref())
        .await
        .map_err(|e| JsonRpcError::internal(format!("abandoned_paths: {e}")))?;

    let mut out = Vec::new();
    for n in &nodes {
        if provenance_page_readable(indexer, &caller, &n.page_id).await? {
            out.push(json!({ "page_id": n.page_id, "skill": n.skill, "via": n.via }));
        }
    }
    Ok(json!({ "nodes": out }))
}

#[derive(Deserialize)]
struct ProvenancePathArgs {
    from_page: String,
    to_page: String,
    #[serde(default)]
    direction: Option<String>,
    #[serde(default)]
    relations: Option<Vec<String>>,
    #[serde(default)]
    max_hops: Option<u32>,
}

async fn tool_provenance_path(
    indexer: &Indexer,
    caller: AclCaller<'_>,
    args: Value,
) -> Result<Value, JsonRpcError> {
    let a: ProvenancePathArgs = parse_args(args, "provenance_path")?;
    let dir = match a.direction.as_deref().unwrap_or("up") {
        "up" => GraphDir::Up,
        "down" => GraphDir::Down,
        other => {
            return Err(JsonRpcError::invalid_params(format!(
                "provenance_path direction `{other}`; expected up|down"
            )));
        }
    };
    let relations = a.relations.unwrap_or_default();
    let rel_opt = (!relations.is_empty()).then_some(relations.as_slice());
    let found = indexer
        .provenance_path(
            &a.from_page,
            &a.to_page,
            dir,
            rel_opt,
            a.max_hops.unwrap_or(PROVENANCE_DEFAULT_HOPS),
        )
        .await
        .map_err(|e| JsonRpcError::internal(format!("provenance_path: {e}")))?;

    // Fail-closed: a path is disclosed only if EVERY node on it is readable.
    // A single private node on the route returns `reachable: false` with no
    // path — never confirm a connection that runs through a hidden record.
    let none = json!({ "reachable": false, "path": [], "depth": 0 });
    let Some(p) = found else {
        return Ok(none);
    };
    for pid in &p.path {
        if !provenance_page_readable(indexer, &caller, pid).await? {
            return Ok(none);
        }
    }
    Ok(json!({ "reachable": true, "path": p.path, "depth": p.depth }))
}

#[derive(Deserialize)]
struct SearchArgs {
    /// Single query string (unchanged). Optional now that `queries`
    /// exists; at least one of `q` / `queries` must be present.
    #[serde(default)]
    q: Option<String>,
    /// Multi-query variants (#217 Part 2). When supplied, each variant
    /// is embedded and run through both lanes; their ACL-filtered
    /// candidate lists are RRF-fused into one ranking before rerank.
    #[serde(default)]
    queries: Option<Vec<String>>,
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
    /// Restrict the search to a single page's blocks (relevance heatmap).
    #[serde(default)]
    page_id: Option<String>,
}

fn default_k() -> usize {
    10
}

/// Upper bound on query variants fused in one `search` call — guards
/// against an unbounded fan-out of first-stage retrievals.
const MAX_QUERY_VARIANTS: usize = 8;

/// The de-duplicated, order-preserving list of query variants to run.
/// Falls back to the scalar `q` when `queries` is absent/empty; errors
/// when neither yields a non-empty string. Capped at
/// [`MAX_QUERY_VARIANTS`].
fn effective_queries(a: &SearchArgs) -> Result<Vec<String>, JsonRpcError> {
    let mut variants: Vec<String> = a
        .queries
        .clone()
        .unwrap_or_default()
        .into_iter()
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .collect();
    if variants.is_empty()
        && let Some(q) = a.q.as_deref().map(str::trim).filter(|s| !s.is_empty())
    {
        variants.push(q.to_owned());
    }
    if variants.is_empty() {
        return Err(JsonRpcError::invalid_params(
            "search: provide `q` or a non-empty `queries`",
        ));
    }
    // Drop duplicate phrasings so the same query can't double-weight a
    // page through RRF; preserve first-seen order.
    let mut seen = std::collections::HashSet::new();
    variants.retain(|v| seen.insert(v.clone()));
    variants.truncate(MAX_QUERY_VARIANTS);
    Ok(variants)
}

async fn tool_search(
    indexer: &Indexer,
    caller: AclCaller<'_>,
    args: Value,
) -> Result<Value, JsonRpcError> {
    let a: SearchArgs = parse_args(args, "search")?;
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

    // Query variants (#217 Part 2): one for a scalar `q`, or several for
    // a `queries` list. Their candidate lists are fused below.
    let variants = effective_queries(&a)?;

    // When the rerank stage is on, fetch a larger fused candidate pool
    // so the cross-encoder has more than the caller's `k` to reorder;
    // we truncate back to `k` after reranking. With rerank off this is
    // `a.k`, so the single-variant path stays byte-identical to before.
    let pool = indexer.rerank_candidate_pool(a.k);

    // The SQL-view lane is skipped when the search is restricted to skills,
    // OR when the caller set constraints the late-materialised lane does not
    // yet honor (`as_of` time-travel, `scenario` overlay, frontmatter
    // `filter`, `page_id`) — fusing unconstrained SQL hits would violate
    // them, so skip the lane (conservative + correct) until it can apply
    // the constraints.
    let constrained =
        a.as_of.is_some() || a.scenario.is_some() || filter.is_some() || a.page_id.is_some();
    let sql_lane_enabled = !matches!(pt, Some(PageType::Skill)) && !constrained;
    let page_id = a.page_id.as_deref().filter(|s| !s.is_empty());

    // Run every variant through both lanes. INV-ACL-FUSION (spike S3):
    // EVERY lane's contribution — for EVERY variant — is ACL-filtered
    // BEFORE it enters the fusion. Deterministic ACL drops owner-private
    // hits the caller does not own (admin bypasses); skill pages are the
    // public catalogue.
    let mut lanes: Vec<Vec<escurel_index::SearchHit>> = Vec::with_capacity(variants.len() * 2);
    for q in &variants {
        let native = indexer
            .search_with(
                q,
                pool,
                pt,
                a.skill.as_deref(),
                a.as_of.as_deref(),
                a.scenario.as_deref(),
                granularity,
                filter,
                page_id,
            )
            .await
            .map_err(|e| JsonRpcError::internal(format!("search: {e}")))?;
        lanes.push(acl_filter_hits(indexer, &caller, native).await?);

        if sql_lane_enabled {
            let candidates = indexer
                .sql_view_search_candidates(q, a.skill.as_deref())
                .await
                .map_err(|e| JsonRpcError::internal(format!("search sql lane: {e}")))?;
            let sql_allowed = acl_filter_hits(indexer, &caller, candidates).await?;
            if !sql_allowed.is_empty() {
                lanes.push(sql_allowed);
            }
        }
    }

    // A single lane (one variant, no SQL contribution) is returned verbatim
    // — the markdown-only single-query behaviour is byte-identical to before.
    // Otherwise RRF-fuse all variant × lane lists into one ranking (over the
    // wider `pool` so the rerank stage sees them all).
    let fused = if lanes.len() == 1 {
        lanes.pop().unwrap_or_default()
    } else {
        rrf_fuse_many(lanes, pool)
    };

    // Cross-encoder rerank — runs AFTER the per-lane ACL filter and RRF
    // fusion (INV-ACL-FUSION): it only reorders rows the caller may
    // already see. A no-op when rerank is disabled. Reranks against the
    // primary variant, then truncate to the caller's `k`.
    let mut final_hits = indexer
        .rerank_hits(&variants[0], fused)
        .await
        .map_err(|e| JsonRpcError::internal(format!("search rerank: {e}")))?;
    final_hits.truncate(a.k);

    let out: Vec<Value> = final_hits
        .iter()
        .map(|h| {
            json!({
                "page_id": h.page_id,
                "slug": h.slug,
                "skill": h.skill,
                "page_type": page_type_str(h.page_type),
                "anchor": h.anchor,
                "snippet": h.snippet,
                "score": h.score,
                "similarity": h.similarity,
                "frontmatter_excerpt": h.frontmatter_excerpt,
            })
        })
        .collect();
    Ok(json!({
        "hits": out,
        "granularity": granularity.as_str(),
    }))
}

/// Apply the fail-closed per-instance read ACL to one lane's candidates,
/// preserving order. Skill-page hits are the public catalogue (never gated).
/// A SQL-view instance whose `owner_field` cannot be resolved fails closed
/// inside `may_read_instance` (deny to non-admins).
async fn acl_filter_hits(
    indexer: &Indexer,
    caller: &AclCaller<'_>,
    hits: Vec<escurel_index::SearchHit>,
) -> Result<Vec<escurel_index::SearchHit>, JsonRpcError> {
    let mut out = Vec::with_capacity(hits.len());
    for h in hits {
        if h.page_type == PageType::Instance
            && !indexer
                .may_read_instance(caller, &h.skill, &h.frontmatter_excerpt)
                .await
                .map_err(|e| JsonRpcError::internal(format!("search acl: {e}")))?
        {
            continue;
        }
        out.push(h);
    }
    Ok(out)
}

/// Reciprocal-Rank-Fusion of N already-ACL-filtered, already-ranked lanes
/// into a page-grain top-`cap`. Each lane contributes `1/(K_RRF + rank)`
/// per page; the first lane to surface a page is its representative on a
/// collision (lanes are pushed native-before-SQL, primary-variant-first,
/// so the strongest source wins the representative). Generalises the
/// two-lane fuse to the multi-query case (#217 Part 2).
fn rrf_fuse_many(
    lanes: Vec<Vec<escurel_index::SearchHit>>,
    cap: usize,
) -> Vec<escurel_index::SearchHit> {
    use std::collections::HashMap;
    const K_RRF: f64 = 60.0;
    let mut scores: HashMap<String, f64> = HashMap::new();
    let mut rep: HashMap<String, escurel_index::SearchHit> = HashMap::new();
    for lane in lanes {
        for (rank, h) in lane.into_iter().enumerate() {
            *scores.entry(h.page_id.clone()).or_insert(0.0) += 1.0 / (K_RRF + (rank as f64) + 1.0);
            rep.entry(h.page_id.clone()).or_insert(h);
        }
    }
    let mut fused: Vec<escurel_index::SearchHit> = rep
        .into_values()
        .map(|mut h| {
            h.score = scores[&h.page_id];
            h
        })
        .collect();
    fused.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.page_id.cmp(&b.page_id))
    });
    fused.truncate(cap);
    fused
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
    let a: RunStoredQueryArgs = parse_args(args, "run_stored_query")?;
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
struct QueryInstanceArgs {
    /// The query page: a bare id, `query::id`, or its `[[query::id]]`
    /// wikilink. `ref` is the documented key; `query_id` is accepted as an
    /// alias for symmetry with `run_stored_query`.
    #[serde(rename = "ref", alias = "query_id")]
    query_ref: String,
    #[serde(default)]
    params: serde_json::Map<String, Value>,
}

/// Normalise a query reference to the bare slug the indexer expects:
/// `[[query::sales]]` / `query::sales` / `sales` all become `sales`.
fn normalize_query_ref(raw: &str) -> String {
    let s = raw.trim();
    let s = s.strip_prefix("[[").unwrap_or(s);
    let s = s.strip_suffix("]]").unwrap_or(s);
    s.strip_prefix("query::").unwrap_or(s).to_owned()
}

async fn tool_query_instance(
    indexer: &Indexer,
    caller: AclCaller<'_>,
    args: Value,
) -> Result<Value, JsonRpcError> {
    let a: QueryInstanceArgs = parse_args(args, "query_instance")?;
    let query_id = normalize_query_ref(&a.query_ref);
    let out = indexer
        .query_instance(&query_id, &a.params, &caller)
        .await
        .map_err(|e| JsonRpcError::internal(format!("query_instance: {e}")))?;
    Ok(json!({
        "rows": out.rows,
        "schema": out.schema.iter().map(|c| json!({
            "name": c.name,
            "type": c.type_name,
        })).collect::<Vec<_>>(),
        "truncated": out.truncated,
    }))
}

#[derive(Deserialize)]
struct ValidateArgs {
    content: String,
    #[serde(default)]
    as_page_id: Option<String>,
}

async fn tool_validate(indexer: &Indexer, args: Value) -> Result<Value, JsonRpcError> {
    let a: ValidateArgs = parse_args(args, "validate")?;
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
    /// Optimistic-concurrency guard (#246): the version the client last read.
    /// When the head has advanced past it, the write conflicts.
    #[serde(default)]
    base_version: Option<String>,
    /// Optional provenance passthrough (#246): a runner-orchestrated write
    /// carries its `provenance.workflow`/`runner` block. Its presence suppresses
    /// the opt-in `page-edited` event (the cascade already handles those writes),
    /// so only genuine out-of-band edits trigger the eager improvement pass.
    #[serde(default)]
    provenance: Option<Value>,
}

async fn tool_update_page(
    state: &crate::server::AppState,
    indexer: &Indexer,
    caller: AclCaller<'_>,
    write_acl: crate::server::WriteAclMode,
    args: Value,
) -> Result<Value, JsonRpcError> {
    let a: UpdatePageArgs = parse_args(args, "update_page")?;

    // Read-only-backend guard (REQ-BK-03): reject an attempt to write backend
    // data for a non-writable backend (creating a sql_view/document instance
    // via update_page, or stripping its backend_ref) with a typed
    // `backend_read_only` Issue. Overlay co-authoring stays allowed.
    if let Some(reason) = indexer
        .backend_read_only_rejection(&a.page_id, &a.content)
        .await
        .map_err(|e| JsonRpcError::internal(format!("update_page backend guard: {e}")))?
    {
        return Ok(json!({
            "ok": false,
            "issues": [{
                "severity": "error",
                "code": "backend_read_only",
                "location": "frontmatter.backend_ref",
                "message": reason,
            }],
        }));
    }

    // Base-layer guard (REQ-LAYER-02): a page imported from a subscribed
    // pack (`layer: base@<pack>@<version>`) is read-only at this node, and
    // `update_page` may not fabricate one. Same dispatch seam as the
    // backend guard above, lifted from per-backend-kind to per-page-layer.
    if let Some(reason) = indexer
        .layer_read_only_rejection(&a.page_id, &a.content)
        .await
        .map_err(|e| JsonRpcError::internal(format!("update_page layer guard: {e}")))?
    {
        return Ok(json!({
            "ok": false,
            "issues": [{
                "severity": "error",
                "code": "layer_read_only",
                "location": "frontmatter.layer",
                "message": reason,
            }],
        }));
    }

    // Curator marker guard (REQ-PROMO-01 / AT-PROMO-2): `promotable:
    // true` gates what may leave this node through the promotion
    // harvest, so only a curator (admin in the v1 two-role model) may
    // write it. Fail-closed: any non-admin draft carrying a truthy
    // `promotable` refuses — an agent can neither self-promote a page
    // nor keep the marker alive by re-writing a curated page.
    if !caller.is_admin
        && escurel_md::parse(&a.content).is_ok_and(|p| {
            p.frontmatter
                .fields
                .get("promotable")
                .and_then(escurel_md::YamlValue::as_bool)
                == Some(true)
        })
    {
        return Ok(json!({
            "ok": false,
            "issues": [{
                "severity": "error",
                "code": "promotable_requires_curator",
                "location": "frontmatter.promotable",
                "message": "`promotable: true` marks content eligible to leave this \
                            node through the promotion harvest; only a curator \
                            (admin role) may set or keep it",
            }],
        }));
    }

    // Shadow-creation gate (REQ-LAYER-03 / agy review): an overlay skill
    // page with a base skill's id changes which DEFINITION governs that
    // id's instances — backend binding, ACL, required_frontmatter. That
    // is curator work; an unprivileged agent authoring one would hijack
    // pack governance. Keyed on the draft's own skill id vs the indexed
    // base pages, so it also holds for the auto-merge path (the id is
    // identical on every side of a merge).
    if !caller.is_admin
        && let Ok(parsed) = escurel_md::parse(&a.content)
        && parsed.frontmatter.page_type == PageType::Skill
    {
        let skill_id = parsed
            .frontmatter
            .fields
            .get("id")
            .and_then(escurel_md::YamlValue::as_str)
            .unwrap_or_default()
            .to_owned();
        if !skill_id.is_empty()
            && indexer
                .skill_page_conflict(&skill_id, &a.page_id)
                .await
                .map_err(|e| JsonRpcError::internal(format!("update_page shadow gate: {e}")))?
                .is_some()
        {
            return Ok(json!({
                "ok": false,
                "issues": [{
                    "severity": "error",
                    "code": "shadow_requires_curator",
                    "location": "frontmatter.id",
                    "message": format!(
                        "skill `{skill_id}` is provided by a subscribed pack; authoring \
                         an overlay that shadows it changes which definition governs \
                         its instances — only a curator (admin role) may do that"
                    ),
                }],
            }));
        }
    }

    // Deterministic per-instance WRITE ACL (symmetric to the read ACL):
    // only the resolved owner (or admin) may mutate an owner-private
    // instance; public/no-owner instances are admin-write-only. `Off`
    // skips; `Log` records a would-be denial but allows; `Enforce` rejects.
    if write_acl != crate::server::WriteAclMode::Off {
        let allowed = indexer
            .may_write_page(&caller, &a.page_id, &a.content)
            .await
            .map_err(|e| JsonRpcError::internal(format!("update_page acl: {e}")))?;
        if !allowed {
            if write_acl == crate::server::WriteAclMode::Log {
                tracing::warn!(
                    subject = %caller.subject,
                    page_id = %a.page_id,
                    "write-ACL would deny this write (log mode) — allowing"
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
                            caller.subject, a.page_id
                        ),
                    }],
                }));
            }
        }
    }

    // #246 optimistic concurrency + monotonic versions + CRDT auto-merge. The
    // page version is `v<max_hlc>` from the CRDT backend (shared with the
    // apply_op session space). When a client sends a stale `base_version` the
    // head has advanced concurrently; rather than clobber or immediately
    // reject, attempt a Loro three-way auto-merge of (base → head) and
    // (base → incoming). A clean merge is persisted (`auto_merged: true`); an
    // unmergeable one falls back to `{ok:false, code:conflict, head_content}`
    // for the client to re-draft. Enforced only when a CRDT backend is wired;
    // otherwise behaviour is unchanged.
    //
    // The gate makes check-then-write atomic: the staleness decision, the
    // indexed write, and the `new_version` assignment must not interleave
    // with another `update_page`, or N simultaneous writes carrying the
    // same stale base all pass validation and silently last-write-win
    // (observed downstream: 20 racing read-modify-writes converged to one
    // survivor). Held to the end of the version bump below; every early
    // return releases it on drop.
    let _cas_gate = state.update_page_gate.lock().await;
    let head_hlc = match state.crdt_backend.as_ref() {
        Some(b) => u64::try_from(b.max_hlc(&a.page_id).await.unwrap_or(0)).unwrap_or(0),
        None => 0,
    };
    // The content we ultimately persist — an auto-merge may replace the raw
    // incoming draft with the merged result.
    let mut content_to_write = a.content.clone();
    let mut auto_merged = false;
    if let (Some(backend), Some(base)) = (state.crdt_backend.as_ref(), a.base_version.as_deref()) {
        let head = Version::from_op_count(head_hlc);
        if base != head.as_str() {
            match try_auto_merge(backend, &a.page_id, base, &a.content).await {
                Some(merged) => {
                    content_to_write = merged;
                    auto_merged = true;
                }
                None => {
                    let head_content = hydrate_content(backend, &a.page_id).await.ok().flatten();
                    return Ok(json!({
                        "ok": false,
                        "issues": [{
                            "severity": "error",
                            "code": "conflict",
                            "location": "base_version",
                            "message": format!(
                                "base_version {base} is stale (head is {}) and the edits could \
                                 not be auto-merged; re-draft against head_content",
                                head.as_str()
                            ),
                        }],
                        "head_content": head_content,
                    }));
                }
            }
        }
    }

    // Re-run the write guards on the MERGED artifact (agy review): the
    // draft-side checks above saw `a.content`, but a clean auto-merge
    // persists `content_to_write`, whose frontmatter can carry keys the
    // head gained since the caller's base — including a laundered
    // `layer: base@…` or `promotable: true`. Whatever produced the
    // final content, what persists must pass the same gates.
    if auto_merged {
        if let Some(reason) = indexer
            .layer_read_only_rejection(&a.page_id, &content_to_write)
            .await
            .map_err(|e| JsonRpcError::internal(format!("update_page merged layer guard: {e}")))?
        {
            return Ok(json!({
                "ok": false,
                "issues": [{
                    "severity": "error",
                    "code": "layer_read_only",
                    "location": "frontmatter.layer",
                    "message": reason,
                }],
            }));
        }
        if !caller.is_admin
            && escurel_md::parse(&content_to_write).is_ok_and(|p| {
                p.frontmatter
                    .fields
                    .get("promotable")
                    .and_then(escurel_md::YamlValue::as_bool)
                    == Some(true)
            })
        {
            return Ok(json!({
                "ok": false,
                "issues": [{
                    "severity": "error",
                    "code": "promotable_requires_curator",
                    "location": "frontmatter.promotable",
                    "message": "the auto-merged result would persist `promotable: true`; \
                                only a curator (admin role) may write the marker — \
                                re-draft against the current head",
                }],
            }));
        }
    }

    match indexer.update_page(&a.page_id, &content_to_write).await {
        Ok(()) => {
            // Advance the monotonic version: snapshot the new whole-page content
            // at the next hlc so `max_hlc` (and any later `base_version` read)
            // reflects this write. The apply_op session path reads the same
            // space, so co-authoring and whole-page writes share versions.
            let new_version = if let Some(backend) = state.crdt_backend.as_ref() {
                let next = head_hlc + 1;
                let bytes = snapshot_bytes_from_markdown(&content_to_write)
                    .map_err(|e| JsonRpcError::internal(format!("update_page crdt: {e}")))?;
                let _ = backend
                    .snapshot(&a.page_id, next as i64, &Snapshot::new(bytes))
                    .await;
                Version::from_op_count(next).as_str().to_owned()
            } else {
                "v1".to_owned()
            };

            // WI-6 absorption instrumentation: count the CONFIRMED write by
            // origin — the same runner/workflow-provenance discriminator the
            // page-edited suppression below uses. The runner/human ratio over
            // time is the interlocked-loops convergence curve.
            let origin = if a
                .provenance
                .as_ref()
                .is_some_and(|p| p.get("workflow").is_some() || p.get("runner").is_some())
            {
                "runner"
            } else {
                "human"
            };
            state.metrics.inc_write(indexer.tenant(), origin);

            // #246 eager per-edit improvement: an OUT-OF-BAND edit (no runner
            // provenance) optionally emits a `page-edited` inbox event so the
            // reactive loop re-lints/re-verifies the touched page. Runner-
            // orchestrated writes carry provenance → suppressed (the cascade
            // already handles them), so there is no loop storm.
            maybe_emit_page_edited(
                state.emit_edit_events,
                indexer,
                &a.page_id,
                a.provenance.as_ref(),
            )
            .await;

            // Announce the confirmed write as an inbox event, so a consumer
            // can be woken by a write instead of polling for it.
            emit_page_event(indexer, state.webhook.as_ref(), &a, &new_version);
            Ok(json!({
                "ok": true,
                "issues": [],
                "new_version": new_version,
                "auto_merged": auto_merged,
            }))
        }
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

#[derive(Deserialize)]
struct DeletePageArgs {
    page_id: String,
    /// Optimistic-concurrency guard (#300, symmetric with `update_page`): the
    /// version the client last read. A stale value conflicts.
    #[serde(default)]
    base_version: Option<String>,
}

#[derive(serde::Deserialize)]
struct MovePageArgs {
    from: String,
    to: String,
}

/// `move_page`: rename a page id, leaving nothing behind.
///
/// Distinct from `delete_page` on purpose. A delete is a *retraction* and
/// keeps the markdown as the audit record; a move is a *rename* and must not,
/// or every restructure litters the canonical store with husks. Restructuring
/// one tenant's 59 instance ids with update+delete left 59 of them.
async fn tool_move_page(
    state: &crate::server::AppState,
    indexer: &Indexer,
    caller: AclCaller<'_>,
    write_acl: crate::server::WriteAclMode,
    args: Value,
) -> Result<Value, JsonRpcError> {
    let a: MovePageArgs = parse_args(args, "move_page")?;

    let Some(existing) = indexer
        .read_page_markdown(&a.from)
        .await
        .map_err(|e| JsonRpcError::internal(format!("move_page read: {e}")))?
    else {
        return Ok(json!({
            "ok": false,
            "issues": [{
                "severity": "error",
                "code": "not_found",
                "location": "from",
                "message": format!("no page `{}` to move", a.from),
            }],
        }));
    };

    // Same gates as delete_page, evaluated against the stored page: a move is
    // a write to both ids, so a backend- or layer-managed page is off limits.
    if let Some(reason) = indexer
        .backend_read_only_rejection(&a.from, &existing)
        .await
        .map_err(|e| JsonRpcError::internal(format!("move_page backend guard: {e}")))?
    {
        return Ok(json!({
            "ok": false,
            "issues": [{
                "severity": "error",
                "code": "backend_read_only",
                "location": "frontmatter.backend_ref",
                "message": reason,
            }],
        }));
    }
    if let Some(reason) = indexer
        .layer_read_only_rejection(&a.from, &existing)
        .await
        .map_err(|e| JsonRpcError::internal(format!("move_page layer guard: {e}")))?
    {
        return Ok(json!({
            "ok": false,
            "issues": [{
                "severity": "error",
                "code": "layer_read_only",
                "location": "frontmatter",
                "message": reason,
            }],
        }));
    }
    if write_acl != crate::server::WriteAclMode::Off {
        let allowed = indexer
            .may_write_page(&caller, &a.from, &existing)
            .await
            .map_err(|e| JsonRpcError::internal(format!("move_page acl: {e}")))?;
        if !allowed {
            if write_acl == crate::server::WriteAclMode::Log {
                tracing::warn!(
                    subject = %caller.subject,
                    page_id = %a.from,
                    "write-ACL would deny this move (log mode) — allowing"
                );
            } else {
                return Ok(json!({
                    "ok": false,
                    "issues": [{
                        "severity": "error",
                        "code": "forbidden",
                        "location": "frontmatter",
                        "message": format!(
                            "move denied: caller `{}` does not own instance `{}`",
                            caller.subject, a.from
                        ),
                    }],
                }));
            }
        }
    }

    match indexer.move_page(&a.from, &a.to).await {
        Ok(true) => {
            state.metrics.inc_write(indexer.tenant(), "human");
            Ok(json!({ "ok": true, "issues": [], "from": a.from, "to": a.to }))
        }
        Ok(false) => Ok(json!({
            "ok": false,
            "issues": [{
                "severity": "error",
                "code": "not_found",
                "location": "from",
                "message": format!("no page `{}` to move", a.from),
            }],
        })),
        Err(IndexerError::PageExists { page_id }) => Ok(json!({
            "ok": false,
            "issues": [{
                "severity": "error",
                "code": "conflict",
                "location": "to",
                "message": format!("a live page already exists at `{page_id}`"),
            }],
        })),
        Err(IndexerError::MetaSkillProtected { reason }) => Ok(json!({
            "ok": false,
            "issues": [{
                "severity": "error",
                "code": "meta_skill_protected",
                "location": "frontmatter",
                "message": reason,
            }],
        })),
        Err(e) => Err(JsonRpcError::internal(format!("move_page: {e}"))),
    }
}

/// #300 `delete_page`: soft-delete (archive) a markdown page/instance. Mirrors
/// `update_page`'s gates — backend/layer read-only, write-ACL, meta-skill — but
/// evaluates them against the STORED page, since a delete carries no new draft.
/// The page is retracted from discovery (index rows + link edges dropped) while
/// its canonical markdown is retained, re-stamped `archived: true`, for audit.
async fn tool_delete_page(
    state: &crate::server::AppState,
    indexer: &Indexer,
    caller: AclCaller<'_>,
    write_acl: crate::server::WriteAclMode,
    args: Value,
) -> Result<Value, JsonRpcError> {
    let a: DeletePageArgs = parse_args(args, "delete_page")?;

    // Fetch the stored markdown; a missing page is a typed `not_found`, not a
    // 500. Idempotent: a second delete (page already retracted) also
    // reports `not_found`.
    let Some(existing) = indexer
        .read_page_markdown(&a.page_id)
        .await
        .map_err(|e| JsonRpcError::internal(format!("delete_page read: {e}")))?
    else {
        return Ok(json!({
            "ok": false,
            "issues": [{
                "severity": "error",
                "code": "not_found",
                "location": "page_id",
                "message": format!("no page `{}` to delete", a.page_id),
            }],
        }));
    };

    // Read-only-backend guard: a sql_view/document instance is managed by its
    // backend, not the markdown write surface — deleting the overlay here
    // would desync it. Evaluated against the stored content.
    if let Some(reason) = indexer
        .backend_read_only_rejection(&a.page_id, &existing)
        .await
        .map_err(|e| JsonRpcError::internal(format!("delete_page backend guard: {e}")))?
    {
        return Ok(json!({
            "ok": false,
            "issues": [{
                "severity": "error",
                "code": "backend_read_only",
                "location": "frontmatter.backend_ref",
                "message": reason,
            }],
        }));
    }

    // Base-layer guard: a page imported from a subscribed pack is read-only at
    // this node — it cannot be deleted here (unsubscribe the pack instead).
    if let Some(reason) = indexer
        .layer_read_only_rejection(&a.page_id, &existing)
        .await
        .map_err(|e| JsonRpcError::internal(format!("delete_page layer guard: {e}")))?
    {
        return Ok(json!({
            "ok": false,
            "issues": [{
                "severity": "error",
                "code": "layer_read_only",
                "location": "frontmatter.layer",
                "message": reason,
            }],
        }));
    }

    // Write ACL: a delete is an overwrite of the existing page, so the caller
    // must own it (or be admin). Passing the stored content as the write
    // content yields the own-the-existing-page (Verb::Update) decision.
    if write_acl != crate::server::WriteAclMode::Off {
        let allowed = indexer
            .may_write_page(&caller, &a.page_id, &existing)
            .await
            .map_err(|e| JsonRpcError::internal(format!("delete_page acl: {e}")))?;
        if !allowed {
            if write_acl == crate::server::WriteAclMode::Log {
                tracing::warn!(
                    subject = %caller.subject,
                    page_id = %a.page_id,
                    "write-ACL would deny this delete (log mode) — allowing"
                );
            } else {
                return Ok(json!({
                    "ok": false,
                    "issues": [{
                        "severity": "error",
                        "code": "forbidden",
                        "location": "frontmatter",
                        "message": format!(
                            "delete denied: caller `{}` does not own instance `{}`",
                            caller.subject, a.page_id
                        ),
                    }],
                }));
            }
        }
    }

    // Optimistic concurrency (#300, symmetric with update_page): a stale
    // `base_version` means the page changed since the caller read it — refuse
    // rather than retract a page they have not seen. Held under the same CAS
    // gate so the check-then-delete cannot interleave with an update_page.
    let _cas_gate = state.update_page_gate.lock().await;
    if let (Some(backend), Some(base)) = (state.crdt_backend.as_ref(), a.base_version.as_deref()) {
        let head_hlc = u64::try_from(backend.max_hlc(&a.page_id).await.unwrap_or(0)).unwrap_or(0);
        let head = Version::from_op_count(head_hlc);
        if base != head.as_str() {
            return Ok(json!({
                "ok": false,
                "issues": [{
                    "severity": "error",
                    "code": "conflict",
                    "location": "base_version",
                    "message": format!(
                        "base_version {base} is stale (head is {}); re-read before deleting",
                        head.as_str()
                    ),
                }],
            }));
        }
    }

    match indexer.delete_page(&a.page_id).await {
        Ok(true) => {
            state.metrics.inc_write(indexer.tenant(), "human");
            Ok(json!({ "ok": true, "issues": [], "page_id": a.page_id }))
        }
        // The page vanished between the read above and here (racing delete).
        Ok(false) => Ok(json!({
            "ok": false,
            "issues": [{
                "severity": "error",
                "code": "not_found",
                "location": "page_id",
                "message": format!("no page `{}` to delete", a.page_id),
            }],
        })),
        Err(IndexerError::MetaSkillProtected { reason }) => Ok(json!({
            "ok": false,
            "issues": [{
                "severity": "error",
                "code": "meta_skill_protected",
                "location": "frontmatter",
                "message": reason,
            }],
        })),
        Err(e) => Err(JsonRpcError::internal(format!("delete_page: {e}"))),
    }
}

/// #246 CRDT three-way auto-merge for a stale `base_version`. Returns the
/// validated merged markdown to persist, or `None` when the write should fall
/// back to a plain conflict.
///
/// `None` (→ conflict) is returned when:
/// * `base_version` isn't a `"v<n>"` we can locate, or
/// * no snapshot was stored at that hlc (the base history is gone — e.g. it was
///   a bare session op-count, not an `update_page` snapshot), or
/// * the current head content can't be hydrated, or
/// * the Loro merge itself errors, or
/// * the merged page no longer parses, or its frontmatter matches *neither*
///   side (an interleaved / corrupted frontmatter — never persisted).
///
/// Body interleaving from overlapping same-region edits is *accepted* — that is
/// the CRDT merge semantics; the frontmatter guard only protects page identity.
async fn try_auto_merge(
    backend: &std::sync::Arc<dyn CrdtBackend>,
    page_id: &str,
    base_version: &str,
    incoming: &str,
) -> Option<String> {
    // Map base_version "vN" -> the snapshot at hlc N that the client branched
    // from. Every update_page write snapshots at its version's hlc, so this
    // reconstructs the exact base for the three-way merge.
    let base_hlc = i64::try_from(Version::parse_op_count(base_version)?).ok()?;
    let base_snapshot = backend
        .snapshot_at(page_id, base_hlc)
        .await
        .ok()
        .flatten()?;
    let head_content = hydrate_content(backend, page_id).await.ok().flatten()?;

    let merged = three_way_merge(&base_snapshot, &head_content, incoming).ok()?;

    // Safety net: the merged page must still parse AND keep one side's
    // frontmatter intact. A body-only merge (the common case) leaves the
    // frontmatter equal to both sides; a one-sided frontmatter change survives
    // via the CRDT; only a genuine both-sides frontmatter divergence (or a
    // corrupt interleave) fails both equalities → conflict.
    let merged_fm = escurel_md::parse(&merged).ok()?.frontmatter.fields;
    let incoming_fm = escurel_md::parse(incoming).ok()?.frontmatter.fields;
    let head_fm = escurel_md::parse(&head_content).ok()?.frontmatter.fields;
    if merged_fm == incoming_fm || merged_fm == head_fm {
        Some(merged)
    } else {
        None
    }
}

/// #246 eager per-edit improvement. When `ESCUREL_EMIT_EDIT_EVENTS` is enabled
/// and the write carries NO runner/workflow provenance (a genuine out-of-band
/// edit), capture a `page-edited` inbox event for the touched page so the
/// runner re-lints/re-verifies it. A provenance-carrying (runner-orchestrated)
/// write is suppressed — the cascade already handles it — so the improvement
/// loop's own writes can't storm. Best-effort: a failure is logged, not fatal.
async fn maybe_emit_page_edited(
    enabled: bool,
    indexer: &Indexer,
    page_id: &str,
    provenance: Option<&Value>,
) {
    let runner_write =
        provenance.is_some_and(|p| p.get("workflow").is_some() || p.get("runner").is_some());
    if !enabled || runner_write {
        return;
    }
    if let Err(e) = indexer
        .capture_event(escurel_index::events::NewEvent {
            event_id: None,
            at: None,
            source: "page-edited".to_owned(),
            mime: "text/plain".to_owned(),
            label_skill: "page-edited".to_owned(),
            instance_page_id: Some(page_id.to_owned()),
            title: format!("edited {page_id}"),
            body: format!("Page {page_id} was edited out of band; re-verify."),
            provenance: Some(json!({ "edit": { "page": page_id } })),
        })
        .await
    {
        tracing::warn!(page_id, error = %e, "page-edited event capture failed");
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

async fn tool_append_message(
    indexer: &Indexer,
    caller: AclCaller<'_>,
    write_acl: crate::server::WriteAclMode,
    args: Value,
) -> Result<Value, JsonRpcError> {
    let a: AppendMessageArgs = parse_args(args, "append_message")?;

    // Chat-surface ACL: only the chat group's owner (or admin) may append.
    if write_acl != crate::server::WriteAclMode::Off {
        let allowed = indexer
            .may_access_chat(&caller, &a.chat_group_id)
            .await
            .map_err(|e| JsonRpcError::internal(format!("append_message acl: {e}")))?;
        if !allowed {
            if write_acl == crate::server::WriteAclMode::Log {
                tracing::warn!(
                    subject = %caller.subject, chat_group_id = %a.chat_group_id,
                    "chat-ACL would deny this append (log mode) — allowing"
                );
            } else {
                return Err(JsonRpcError::forbidden(format!(
                    "append denied: caller `{}` does not own chat `{}`",
                    caller.subject, a.chat_group_id
                )));
            }
        }
    }

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

async fn tool_list_messages(
    indexer: &Indexer,
    caller: AclCaller<'_>,
    write_acl: crate::server::WriteAclMode,
    args: Value,
) -> Result<Value, JsonRpcError> {
    let a: ListMessagesArgs = parse_args(args, "list_messages")?;

    // Chat-surface ACL: only the chat group's owner (or admin) may read its
    // history. A denial returns an EMPTY page (non-leaking, like expand→null),
    // never another member's transcript.
    if write_acl != crate::server::WriteAclMode::Off {
        let allowed = indexer
            .may_access_chat(&caller, &a.chat_group_id)
            .await
            .map_err(|e| JsonRpcError::internal(format!("list_messages acl: {e}")))?;
        if !allowed {
            if write_acl == crate::server::WriteAclMode::Log {
                tracing::warn!(
                    subject = %caller.subject, chat_group_id = %a.chat_group_id,
                    "chat-ACL would deny this read (log mode) — allowing"
                );
            } else {
                return Ok(json!({ "messages": [] }));
            }
        }
    }
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
    let a: CaptureEventArgs = parse_args(args, "capture_event")?;
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
    // fire-and-forget; never fails the capture). The gateway is
    // single-tenant per indexer, so `indexer.tenant()` is the
    // authoritative tenant we stamp into the delivered payload (#147).
    if let Some(hook) = webhook {
        hook.notify(event.clone(), indexer.tenant());
    }
    Ok(event)
}

/// Announce a confirmed `update_page` write to the outbound webhook.
///
/// ## Notification, not work
///
/// This does **not** capture an inbox item. The inbox is a work queue: the
/// runner drains it and dispatches each entry. Turning every write into an
/// inbox entry makes a write into work, and then
///
///   * a write announces itself,
///   * the runner dispatches the announcement,
///   * processing it writes again,
///
/// which is unbounded — a page-write carries no cascade lineage for #157's
/// depth and budget caps to bite on. Measured, not theorised: with writes
/// enqueued, one demo run produced 122 announcements for a single skill and
/// starved the real workflow behind a quota gate.
///
/// So a page write is delivered as a **notification only**. Consumers that
/// want to be woken by a write subscribe to the webhook; the work queue
/// keeps its meaning. This is also what makes workflow completion
/// observable at all: a runner-authored write is exactly the interesting
/// one, and any inbox-based scheme has to suppress it to avoid the loop.
///
/// Best-effort and fire-and-forget, like the capture webhook: a failure
/// never touches the write, which already succeeded.
fn emit_page_event(
    indexer: &Indexer,
    webhook: Option<&crate::webhook::Webhook>,
    a: &UpdatePageArgs,
    new_version: &str,
) {
    let Some(hook) = webhook else { return };
    // Instance pages only: the skill lives in the path
    // (`markdown/instances/<skill>/<id>.md`). A skill-page write is not an
    // instance change and is not announced.
    let Some(skill) = a
        .page_id
        .split('/')
        .nth(2)
        .filter(|_| a.page_id.starts_with("markdown/instances/"))
        .map(str::to_string)
    else {
        return;
    };
    hook.notify(
        json!({
            "kind": "page_write",
            "label_skill": skill,
            "instance_page_id": a.page_id,
            "new_version": new_version,
        }),
        indexer.tenant(),
    );
}

#[derive(Deserialize)]
struct ListInboxArgs {
    #[serde(default)]
    limit: Option<usize>,
}

async fn tool_list_inbox(indexer: &Indexer, args: Value) -> Result<Value, JsonRpcError> {
    let a: ListInboxArgs = parse_args(args, "list_inbox")?;
    let events = indexer
        .list_inbox(a.limit)
        .await
        .map_err(|e| JsonRpcError::internal(format!("list_inbox: {e}")))?;
    Ok(json!({ "events": events.iter().map(event_to_json).collect::<Vec<_>>() }))
}

#[derive(Deserialize)]
struct ListEventsArgs {
    #[serde(default)]
    instance_page_id: String,
    #[serde(default)]
    limit: Option<usize>,
    /// By-event lookup. When present, returns just that event (whatever its
    /// status) and `instance_page_id` is ignored.
    #[serde(default)]
    event_id: Option<String>,
}

async fn tool_list_events(indexer: &Indexer, args: Value) -> Result<Value, JsonRpcError> {
    let a: ListEventsArgs = parse_args(args, "list_events")?;

    // `event_id` answers "where did this event go?" — the question an
    // instance-scoped listing cannot ask, because you would need the answer
    // to form the query. An event with no match is an empty list, not an
    // error: "not found" is a legitimate answer here, and the caller
    // distinguishes it from "found, still in the inbox".
    let events = if let Some(event_id) = a.event_id.as_deref() {
        indexer
            .get_event(event_id)
            .await
            .map_err(|e| JsonRpcError::internal(format!("list_events: {e}")))?
            .into_iter()
            .collect::<Vec<_>>()
    } else {
        if a.instance_page_id.is_empty() {
            return Err(JsonRpcError::invalid_params(
                "list_events: one of `instance_page_id` or `event_id` is required".to_owned(),
            ));
        }
        indexer
            .list_events(&a.instance_page_id, a.limit)
            .await
            .map_err(|e| JsonRpcError::internal(format!("list_events: {e}")))?
    };
    Ok(json!({ "events": events.iter().map(event_to_json).collect::<Vec<_>>() }))
}

#[derive(Deserialize)]
struct ListSnapshotsArgs {
    page_id: String,
}

async fn tool_list_snapshots(indexer: &Indexer, args: Value) -> Result<Value, JsonRpcError> {
    let a: ListSnapshotsArgs = parse_args(args, "list_snapshots")?;
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
    let a: AssignEventArgs = parse_args(args, "assign_event")?;
    indexer
        .assign_event(&a.event_id, &a.instance_page_id)
        .await
        .map_err(|e| match e {
            // Caller errors, not server faults: the event does not exist,
            // or another agent already claimed it. `invalid_params` matches
            // how `PackSkillMissing` is surfaced and — unlike `internal` —
            // tells a retrying caller that retrying will not help.
            // Re-assigning to the SAME instance returns Ok(()) and never
            // reaches here, so runner recovery is unaffected.
            IndexerError::EventNotFound { .. } | IndexerError::EventAlreadyAssigned { .. } => {
                JsonRpcError::invalid_params(format!("assign_event: {e}"))
            }
            other => JsonRpcError::internal(format!("assign_event: {other}")),
        })?;
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

// --- session tools (M4.2) --------------------------------------

#[derive(Deserialize)]
struct OpenSessionArgs {
    page_id: String,
}

async fn tool_open_session(
    backend: Option<&Arc<dyn CrdtBackend>>,
    indexer: Option<&Indexer>,
    sessions: Arc<SessionManager>,
    quota: Option<&Arc<QuotaManager>>,
    tenant_id: &str,
    args: Value,
) -> Result<Value, JsonRpcError> {
    let a: OpenSessionArgs = parse_args(args, "open_session")?;
    let backend = backend
        .ok_or_else(|| JsonRpcError::internal("live CRDT mode not enabled on this server"))?;

    // Base-layer guard (REQ-LAYER-02): live co-authoring must not bypass
    // the `update_page` read-only guard — an open session's `apply_op`
    // stream would edit a base page byte by byte. The reserved-prefix
    // half is static (no indexer needed) and race-free.
    if a.page_id
        .starts_with(escurel_index::pack::RESERVED_BASE_PREFIX)
    {
        return Err(JsonRpcError {
            code: -32000,
            message: format!(
                "layer_read_only: page `{}` is under the reserved `{}` namespace — \
                 pack-managed, read-only at this node",
                a.page_id,
                escurel_index::pack::RESERVED_BASE_PREFIX
            ),
        });
    }
    // Session-only servers (`indexer = None`) have no page corpus, so no
    // stored base pages to guard beyond the prefix above.
    if let Some(ix) = indexer {
        let layer = ix
            .page_layer(&a.page_id)
            .await
            .map_err(|e| JsonRpcError::internal(format!("open_session layer guard: {e}")))?;
        if let Some(layer) = layer.filter(|l| l.starts_with("base@")) {
            return Err(JsonRpcError {
                code: -32000,
                message: format!(
                    "layer_read_only: page `{}` is layer `{layer}` — imported from a \
                     subscribed pack and read-only at this node; author an overlay \
                     page to specialise it",
                    a.page_id
                ),
            });
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
    let a: ApplyOpArgs = parse_args(args, "apply_op")?;
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
    let a: CloseSessionArgs = parse_args(args, "close_session")?;
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
    /// The caller is authenticated but not permitted to perform the action
    /// (per-instance / chat ACL denial). App-defined code in the
    /// JSON-RPC implementation-defined server-error range.
    fn forbidden(msg: impl Into<String>) -> Self {
        Self {
            code: -32003,
            message: msg.into(),
        }
    }
    /// A precondition the server can't satisfy (e.g. an admin tool
    /// asked to act on a tenant other than the one this single-tenant
    /// gateway is bound to). Mirrors the old gRPC `FailedPrecondition`.
    fn failed_precondition(msg: impl Into<String>) -> Self {
        Self {
            code: -32002,
            message: msg.into(),
        }
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
        }
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
        }
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
