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
    AclCaller, AppendChatMessage, Capabilities, ChatMessage, Direction, EventInfo, Granularity,
    Indexer, IndexerError, Issue, ListChatMessages, NewEvent, OrderDir, Severity, Visibility,
    derive_attach_alias, is_safe_attach_source,
};
use escurel_md::PageType;
use escurel_quota::{Dimension, QuotaError, QuotaManager};
use escurel_storage::{Key, StoreError};
use escurel_types::{
    AdminLaneBlobResponse, AttachExternalResponse, CompactProgress, EmbeddingReloadResponse,
    ListSkillsResponse, QuotaGetResponse, RebuildProgress, Skill as TypesSkill,
    SkillAcl as TypesSkillAcl, SkillBackend as TypesSkillBackend,
    SkillCapabilities as TypesSkillCapabilities, TenantCreateResponse, TenantDeleteResponse,
    TenantGetResponse, TenantImportResponse, TenantListResponse, TenantSpec as TypesTenantSpec,
    TenantUpdateResponse, WebhookDeliveriesResponse, WebhookDelivery,
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
/// or not a handler matched; and dispatches the deterministic worker (PR-3d)
/// when one did. An unmatched MIME is parked with `no_handler_skill` and the
/// inbox blob is retained (never silently dropped).
#[derive(Deserialize)]
pub(crate) struct IngestRequest {
    blob_id: String,
    content_type: String,
    #[serde(default)]
    title: Option<String>,
    /// Optional explicit target document skill. When absent, the skill is
    /// resolved from the MIME (REQ-DOC-06). When present it must be a
    /// `document`-backend skill that `accepts` the MIME, else the request is
    /// rejected — this is how an upload reaches a *specific* document skill
    /// (e.g. a per-fraktion collection) when several accept the same MIME.
    #[serde(default)]
    skill: Option<String>,
}

/// The authenticated caller of an ingest request — the bits needed to enforce
/// create-ACL on an explicit target skill (mirrors the tools path).
struct IngestCaller {
    subject: String,
    /// RBAC token groups (admin role value already stripped).
    groups: Vec<String>,
    is_admin: bool,
}

/// Auth (REQ-NF-07) + per-tenant Writes rate-limit gate shared by `/ingest`
/// and `/ingest/upload`. Returns a cloned indexer handle + the caller (subject,
/// groups, admin) for downstream ACL checks, or an error response.
async fn ingest_gate(
    state: &crate::server::AppState,
    headers: &HeaderMap,
) -> Result<(std::sync::Arc<Indexer>, IngestCaller), axum::response::Response> {
    let auth_ctx = match state.verifier.as_ref() {
        Some(v) => match enforce_auth(v, headers).await {
            Ok(c) => Some(c),
            Err(resp) => return Err(resp),
        },
        None => None,
    };
    let subject = auth_ctx
        .as_ref()
        .map(|c| c.subject.clone())
        .unwrap_or_default();
    // RBAC groups (strip the admin role value so it can't act as a group),
    // mirroring `mcp_inner`. No verifier (dev / on-host mode) → admin bypass.
    let admin_value = state
        .verifier
        .as_ref()
        .map(|v| v.config().admin_role_value.clone());
    let groups: Vec<String> = auth_ctx
        .as_ref()
        .map(|c| {
            c.groups
                .iter()
                .filter(|g| Some(g.as_str()) != admin_value.as_deref())
                .cloned()
                .collect()
        })
        .unwrap_or_default();
    let is_admin = match &auth_ctx {
        Some(c) => matches!(c.role, Role::Admin),
        None => true,
    };
    if let (Some(quota), Some(ctx)) = (state.quota.as_ref(), auth_ctx.as_ref())
        && let Err(err) = quota.try_consume(&ctx.tenant_id, Dimension::Writes)
    {
        return Err((
            StatusCode::TOO_MANY_REQUESTS,
            Json(json!({ "error": "rate_limited", "message": err.to_string() })),
        )
            .into_response());
    }
    match state.indexer.as_ref() {
        Some(i) => Ok((
            std::sync::Arc::clone(i),
            IngestCaller {
                subject,
                groups,
                is_admin,
            },
        )),
        None => Err((
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({ "error": "no indexer wired" })),
        )
            .into_response()),
    }
}

pub(crate) async fn ingest(
    State(state): State<crate::server::AppState>,
    headers: HeaderMap,
    Json(req): Json<IngestRequest>,
) -> axum::response::Response {
    state.metrics.inc_request("/ingest", 200);
    let (indexer, caller) = match ingest_gate(&state, &headers).await {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    record_and_dispatch_ingest(
        &indexer,
        &req.blob_id,
        &req.content_type,
        req.title,
        req.skill.as_deref(),
        &caller,
    )
    .await
}

/// `POST /ingest/upload` — browser-friendly intake: deposit inline base64
/// bytes into the inbox (content-addressed), then run the same ingest path.
/// The SPA can't deposit a content-addressed blob itself; the BFF proxies this
/// with JWT minting.
#[derive(Deserialize)]
pub(crate) struct IngestUploadRequest {
    content_type: String,
    /// base64-encoded file bytes.
    bytes_b64: String,
    #[serde(default)]
    title: Option<String>,
    /// Optional explicit target document skill (see [`IngestRequest::skill`]).
    #[serde(default)]
    skill: Option<String>,
}

pub(crate) async fn ingest_upload(
    State(state): State<crate::server::AppState>,
    headers: HeaderMap,
    Json(req): Json<IngestUploadRequest>,
) -> axum::response::Response {
    state.metrics.inc_request("/ingest/upload", 200);
    let (indexer, caller) = match ingest_gate(&state, &headers).await {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let bytes = match B64.decode(req.bytes_b64.as_bytes()) {
        Ok(b) => b,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": format!("bytes_b64 is not base64: {e}") })),
            )
                .into_response();
        }
    };
    // Per-upload blob-size quota: reject an oversize payload *before* it is
    // deposited, so an upload can never fill the host volume. `0` = unbounded.
    if let Some(quota) = state.quota.as_ref() {
        let cap = quota.max_blob_bytes(indexer.tenant());
        if cap > 0 && bytes.len() as u64 > cap {
            state.metrics.inc_request("/ingest/upload", 413);
            return (
                StatusCode::PAYLOAD_TOO_LARGE,
                Json(json!({
                    "error": "payload_too_large",
                    "message": format!(
                        "upload is {} bytes; the per-upload limit is {cap} bytes",
                        bytes.len()
                    ),
                    "max_bytes": cap,
                })),
            )
                .into_response();
        }
    }
    // Deposit into the inbox before processing (the canonical-before-process
    // step; an upload is never lost).
    let blob = match indexer
        .lane_store()
        .put_inbox_blob(indexer.tenant(), bytes::Bytes::from(bytes), None)
        .await
    {
        Ok(id) => id,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": format!("deposit: {e}") })),
            )
                .into_response();
        }
    };
    record_and_dispatch_ingest(
        &indexer,
        blob.as_str(),
        &req.content_type,
        req.title,
        req.skill.as_deref(),
        &caller,
    )
    .await
}

/// Shared tail: resolve MIME→skill (REQ-DOC-06), record the immutable ingest
/// Event (auditable), then dispatch the worker or park `no_handler_skill`.
async fn record_and_dispatch_ingest(
    indexer: &std::sync::Arc<Indexer>,
    blob_id: &str,
    content_type: &str,
    title: Option<String>,
    target_skill: Option<&str>,
    caller: &IngestCaller,
) -> axum::response::Response {
    let subject = caller.subject.as_str();
    // Resolve the handler skill: an explicit `target_skill` (validated to be a
    // document-backend skill that accepts the MIME) wins; otherwise route by
    // MIME (REQ-DOC-06). An explicit skill that is missing, not a document
    // backend, or does not accept the MIME is a 422 (never silently re-routed
    // — that would land an upload in the wrong, possibly wider-visible skill).
    let handler = match target_skill {
        Some(sk) => {
            let accepts = match indexer.skill_backend(sk).await {
                Ok(b) => {
                    b.kind == escurel_index::backend::BackendKind::Document
                        && b.document
                            .as_ref()
                            .is_some_and(|d| d.accepts.iter().any(|m| m == content_type))
                }
                Err(e) => {
                    return (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        Json(json!({ "error": format!("skill backend: {e}") })),
                    )
                        .into_response();
                }
            };
            if !accepts {
                return (
                    StatusCode::UNPROCESSABLE_ENTITY,
                    Json(json!({
                        "error": "invalid_target_skill",
                        "message": format!(
                            "skill `{sk}` is not a document skill that accepts `{content_type}`"
                        ),
                    })),
                )
                    .into_response();
            }
            // AUTHORIZATION: the caller *chose* this skill, and the document
            // materialise path bypasses the normal `update_page` write gate — so
            // enforce the skill's `create` ACL here. Otherwise an authenticated
            // user could inject a (group-readable) document into a skill they may
            // not write, e.g. another fraktion's collection. The would-be owner
            // (`owner_field` ← subject) is part of the create decision.
            let owner_field = indexer.list_skills().await.ok().and_then(|ss| {
                ss.into_iter()
                    .find(|s| s.id == sk)
                    .and_then(|s| s.owner_field)
            });
            let mut incoming = serde_json::Map::new();
            if let Some(field) = &owner_field
                && !subject.is_empty()
            {
                incoming.insert(field.clone(), json!(subject));
            }
            let acl_caller = AclCaller {
                subject,
                is_admin: caller.is_admin,
                token_groups: &caller.groups,
            };
            let may_create = indexer
                .may_write_instance(&acl_caller, sk, None, &Value::Object(incoming))
                .await
                .unwrap_or(false);
            if !may_create {
                return (
                    StatusCode::FORBIDDEN,
                    Json(json!({
                        "error": "forbidden",
                        "message": format!("not authorised to create documents in skill `{sk}`"),
                    })),
                )
                    .into_response();
            }
            Some(sk.to_owned())
        }
        None => match indexer.document_skill_for_mime(content_type).await {
            Ok(h) => h,
            Err(e) => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({ "error": format!("mime resolution: {e}") })),
                )
                    .into_response();
            }
        },
    };
    let label_skill = handler.clone().unwrap_or_else(|| "ingest".to_owned());
    let event = indexer
        .capture_event(NewEvent {
            event_id: None,
            at: None,
            source: "ingest".to_owned(),
            mime: content_type.to_owned(),
            label_skill,
            instance_page_id: None,
            title: title.unwrap_or_else(|| blob_id.to_owned()),
            body: String::new(),
            provenance: Some(json!({
                "blob_id": blob_id,
                "content_type": content_type,
                "handler_skill": handler,
                "by": subject,
            })),
        })
        .await;
    let event = match event {
        Ok(e) => e,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": format!("record ingest event: {e}") })),
            )
                .into_response();
        }
    };
    match handler {
        Some(skill) => {
            run_document_ingest(indexer, &skill, blob_id, content_type, &event.event_id, subject)
                .await
        }
        None => (
            StatusCode::ACCEPTED,
            Json(json!({
                "event_id": event.event_id,
                "blob_id": blob_id,
                "status": "no_handler",
                "issue": {
                    "code": "no_handler_skill",
                    "message": format!(
                        "no document skill accepts content type `{content_type}`; inbox blob retained"
                    ),
                },
            })),
        )
            .into_response(),
    }
}

/// Run the deterministic ingest worker inline: extract+chunk off the write
/// lock, materialise under a brief lock. v1 uses the born-digital text
/// processor (kreuzberg PDF/DOCX is gated on the MSRV decision).
async fn run_document_ingest(
    indexer: &std::sync::Arc<Indexer>,
    skill: &str,
    blob_id_str: &str,
    content_type: &str,
    event_id: &str,
    subject: &str,
) -> axum::response::Response {
    use escurel_index::backend::{
        ChunkConfig, DeterministicProcessor, DocumentIngestWorker, ExtractConfig, Extractor,
        IngestOutcome, OcrPolicy, PlainTextExtractor,
    };
    use escurel_storage::BlobId;

    let Some(blob_id) = BlobId::parse(blob_id_str) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "invalid blob_id (expected sha256:<hex>)" })),
        )
            .into_response();
    };

    // Chunk knobs from the skill's document binding (defaults when absent).
    let chunk = match indexer.skill_backend(skill).await {
        Ok(b) => b
            .document
            .map(|d| (d.max_chars, d.overlap))
            .unwrap_or((None, None)),
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": format!("skill backend: {e}") })),
            )
                .into_response();
        }
    };
    let defaults = ChunkConfig::default();
    let cfg = ExtractConfig {
        ocr: OcrPolicy::Off,
        chunk: ChunkConfig {
            max_chars: chunk.0.unwrap_or(defaults.max_chars),
            overlap: chunk.1.unwrap_or(defaults.overlap),
        },
    };

    // Deterministic instance id from the content hash (idempotent intake).
    let instance_id = format!("doc-{}", &blob_id.hex()[..12.min(blob_id.hex().len())]);
    let extractor: std::sync::Arc<dyn Extractor> = if content_type.starts_with("text/") {
        std::sync::Arc::new(PlainTextExtractor)
    } else {
        #[cfg(feature = "kreuzberg")]
        {
            std::sync::Arc::new(escurel_index::backend::KreuzbergExtractor)
        }
        #[cfg(not(feature = "kreuzberg"))]
        {
            std::sync::Arc::new(PlainTextExtractor)
        }
    };
    let worker = DocumentIngestWorker::new(
        std::sync::Arc::clone(indexer),
        std::sync::Arc::new(DeterministicProcessor::new(extractor)),
    );

    // Stamp the uploader as the instance owner so owner-scoped document skills
    // work: a personal skill (`read: [owner]`) stays visible only to its
    // uploader, and a group-shared skill (`read: [owner, <group>]`) is owned by
    // the uploader but readable by the group. Resolved from the skill's
    // `owner_field`; skipped for skills without one (or an anonymous caller).
    let extra = match indexer.list_skills().await {
        Ok(skills) => skills
            .into_iter()
            .find(|s| s.id == skill)
            .and_then(|s| s.owner_field)
            .filter(|_| !subject.is_empty())
            .map(|field| json!({ field: subject }))
            .unwrap_or(serde_json::Value::Null),
        Err(_) => serde_json::Value::Null,
    };

    match worker
        .ingest(&blob_id, content_type, skill, &instance_id, &cfg, &extra)
        .await
    {
        Ok(IngestOutcome::Materialised {
            page_id,
            chunk_count,
        }) => (
            StatusCode::ACCEPTED,
            Json(json!({
                "event_id": event_id,
                "blob_id": blob_id_str,
                "handler_skill": skill,
                "status": "materialised",
                "page_id": page_id,
                "chunk_count": chunk_count,
            })),
        )
            .into_response(),
        Ok(IngestOutcome::ExtractionFailed { page_id, reason }) => (
            StatusCode::ACCEPTED,
            Json(json!({
                "event_id": event_id,
                "blob_id": blob_id_str,
                "handler_skill": skill,
                "status": "extraction_failed",
                "page_id": page_id,
                "issue": { "code": "extraction_failed", "message": reason },
            })),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": format!("ingest worker: {e}") })),
        )
            .into_response(),
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
        // Outbound-webhook delivery log (observability). Needs only the
        // webhook handle on AppState, so it routes before the indexer gate.
        "admin_webhook_deliveries" => {
            require_admin(role)?;
            return tool_admin_webhook_deliveries(state, params.arguments);
        }
        _ => {}
    }

    let indexer = state.indexer.as_ref().ok_or_else(|| {
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
        "expand" => tool_expand(indexer, caller, params.arguments).await,
        "neighbours" => tool_neighbours(indexer, caller, params.arguments).await,
        "search" => tool_search(indexer, caller, params.arguments).await,
        "run_stored_query" => {
            // A stored query runs pre-declared arbitrary SQL over the whole
            // corpus and returns arbitrary projected columns (aggregates,
            // joins) — there is no per-row owner to filter on, so the ACL is
            // at the capability level: operator/analytics only.
            require_admin(role)?;
            tool_run_stored_query(indexer, params.arguments).await
        }
        "validate" => tool_validate(indexer, params.arguments).await,
        "update_page" => tool_update_page(indexer, caller, state.write_acl, params.arguments).await,
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
    let a: ResolveArgs = serde_json::from_value(args)
        .map_err(|e| JsonRpcError::invalid_params(format!("resolve: {e}")))?;
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
}

async fn tool_expand(
    indexer: &Indexer,
    caller: AclCaller<'_>,
    args: Value,
) -> Result<Value, JsonRpcError> {
    let a: ExpandArgs = serde_json::from_value(args)
        .map_err(|e| JsonRpcError::invalid_params(format!("expand: {e}")))?;
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
                if let Some(arr) = page["blocks"].as_array().cloned() {
                    let lead: Vec<Value> = arr.into_iter().take(lead_n).collect();
                    page["blocks"] = Value::from(lead);
                }
                page["chunks_total"] = json!(total);
                page["chunks_truncated"] = json!(total > lead_n);
            }
            Ok(page)
        }
    }
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

async fn tool_search(
    indexer: &Indexer,
    caller: AclCaller<'_>,
    args: Value,
) -> Result<Value, JsonRpcError> {
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

    // Native lane: the fused hybrid (vss+fts) hits.
    let native = indexer
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

    // INV-ACL-FUSION (spike S3): every lane's contribution is ACL-filtered
    // BEFORE fusion. Deterministic ACL drops owner-private hits the caller
    // does not own (admin bypasses); skill pages are the public catalogue.
    let native_allowed = acl_filter_hits(indexer, &caller, native).await?;

    // SQL-view lane: late-materialised candidates over each sql_view skill's
    // search_text columns. Skipped when the search is restricted to skills,
    // OR when the caller set constraints the late-materialised lane does not
    // yet honor (`as_of` time-travel, `scenario` overlay, frontmatter
    // `filter`) — fusing unconstrained SQL hits would violate them, so skip
    // the lane (conservative + correct) until it can apply the constraints.
    let constrained = a.as_of.is_some() || a.scenario.is_some() || filter.is_some();
    let sql_allowed = if matches!(pt, Some(PageType::Skill)) || constrained {
        Vec::new()
    } else {
        let candidates = indexer
            .sql_view_search_candidates(&a.q, a.skill.as_deref())
            .await
            .map_err(|e| JsonRpcError::internal(format!("search sql lane: {e}")))?;
        acl_filter_hits(indexer, &caller, candidates).await?
    };

    // No SQL contribution → the native path is returned verbatim (the
    // markdown-only behaviour is byte-identical to before fusion relocation).
    // Otherwise RRF-fuse the two already-filtered lanes.
    let final_hits = if sql_allowed.is_empty() {
        native_allowed
    } else {
        rrf_fuse_lanes(native_allowed, sql_allowed, a.k)
    };

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

/// Reciprocal-Rank-Fusion of two already-ACL-filtered, already-ranked lanes
/// into a page-grain top-`k`. Each lane contributes `1/(K_RRF + rank)` per
/// page; the native lane's hit is the representative when a page appears in
/// both.
fn rrf_fuse_lanes(
    native: Vec<escurel_index::SearchHit>,
    sql: Vec<escurel_index::SearchHit>,
    k: usize,
) -> Vec<escurel_index::SearchHit> {
    use std::collections::HashMap;
    const K_RRF: f64 = 60.0;
    let mut scores: HashMap<String, f64> = HashMap::new();
    let mut rep: HashMap<String, escurel_index::SearchHit> = HashMap::new();
    // Native first, so it wins as the representative on a page collision.
    for lane in [native, sql] {
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
    fused.truncate(k);
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

async fn tool_update_page(
    indexer: &Indexer,
    caller: AclCaller<'_>,
    write_acl: crate::server::WriteAclMode,
    args: Value,
) -> Result<Value, JsonRpcError> {
    let a: UpdatePageArgs = serde_json::from_value(args)
        .map_err(|e| JsonRpcError::invalid_params(format!("update_page: {e}")))?;

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

async fn tool_append_message(
    indexer: &Indexer,
    caller: AclCaller<'_>,
    write_acl: crate::server::WriteAclMode,
    args: Value,
) -> Result<Value, JsonRpcError> {
    let a: AppendMessageArgs = serde_json::from_value(args)
        .map_err(|e| JsonRpcError::invalid_params(format!("append_message: {e}")))?;

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
    let a: ListMessagesArgs = serde_json::from_value(args)
        .map_err(|e| JsonRpcError::invalid_params(format!("list_messages: {e}")))?;

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
    // fire-and-forget; never fails the capture). The gateway is
    // single-tenant per indexer, so `indexer.tenant()` is the
    // authoritative tenant we stamp into the delivered payload (#147).
    if let Some(hook) = webhook {
        hook.notify(event.clone(), indexer.tenant());
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
    args: Value,
) -> Result<Value, JsonRpcError> {
    // Honour the requested tenant: reject a `tenant_id` arg that names
    // a different tenant than this gateway serves, rather than silently
    // returning the caller's own snapshot.
    let req: TenantIdArgs = serde_json::from_value(args)
        .map_err(|e| JsonRpcError::invalid_params(format!("admin_quota: {e}")))?;
    if let Some(indexer) = state.indexer.as_ref() {
        ensure_tenant_matches(indexer, &req.tenant_id)?;
    }
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

#[derive(Deserialize)]
struct WebhookDeliveriesArgs {
    #[serde(default)]
    limit: Option<usize>,
}

/// Recent outbound-webhook delivery outcomes (newest first). Observability
/// for whether captures are reaching the agent runner. `configured: false`
/// when no `ESCUREL_WEBHOOK_URL` is set (nothing is ever sent).
fn tool_admin_webhook_deliveries(
    state: &crate::server::AppState,
    args: Value,
) -> Result<Value, JsonRpcError> {
    let a: WebhookDeliveriesArgs = serde_json::from_value(args)
        .map_err(|e| JsonRpcError::invalid_params(format!("admin_webhook_deliveries: {e}")))?;
    let limit = a.limit.unwrap_or(100).min(200);
    let (configured, records) = match state.webhook.as_ref() {
        Some(w) => (true, w.recent(limit)),
        None => (false, Vec::new()),
    };
    to_value(WebhookDeliveriesResponse {
        configured,
        deliveries: records
            .into_iter()
            .map(|d| WebhookDelivery {
                event_id: d.event_id,
                at_ms: d.at_ms,
                ok: d.ok,
                http_status: d.http_status,
                error: d.error,
            })
            .collect(),
    })
}

async fn tool_admin_audit(indexer: &Indexer, args: Value) -> Result<Value, JsonRpcError> {
    let req: TenantIdArgs = serde_json::from_value(args)
        .map_err(|e| JsonRpcError::invalid_params(format!("admin_audit: {e}")))?;
    ensure_tenant_matches(indexer, &req.tenant_id)?;
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

#[derive(Deserialize)]
struct GroupMemberArgs {
    group_id: String,
    subject: String,
}

#[derive(Deserialize)]
struct ListGroupMembersArgs {
    group_id: String,
}

async fn tool_add_group_member(
    indexer: &Indexer,
    added_by: &str,
    args: Value,
) -> Result<Value, JsonRpcError> {
    let a: GroupMemberArgs = serde_json::from_value(args)
        .map_err(|e| JsonRpcError::invalid_params(format!("add_group_member: {e}")))?;
    indexer
        .add_group_member(&a.group_id, &a.subject, Some(added_by))
        .await
        .map_err(|e| JsonRpcError::internal(format!("add_group_member: {e}")))?;
    Ok(json!({ "ok": true }))
}

async fn tool_remove_group_member(indexer: &Indexer, args: Value) -> Result<Value, JsonRpcError> {
    let a: GroupMemberArgs = serde_json::from_value(args)
        .map_err(|e| JsonRpcError::invalid_params(format!("remove_group_member: {e}")))?;
    indexer
        .remove_group_member(&a.group_id, &a.subject)
        .await
        .map_err(|e| JsonRpcError::internal(format!("remove_group_member: {e}")))?;
    Ok(json!({ "ok": true }))
}

async fn tool_list_group_members(indexer: &Indexer, args: Value) -> Result<Value, JsonRpcError> {
    let a: ListGroupMembersArgs = serde_json::from_value(args)
        .map_err(|e| JsonRpcError::invalid_params(format!("list_group_members: {e}")))?;
    let members = indexer
        .list_group_members(&a.group_id)
        .await
        .map_err(|e| JsonRpcError::internal(format!("list_group_members: {e}")))?;
    let members: Vec<Value> = members
        .into_iter()
        .map(|m| {
            json!({
                "group_id": m.group_id,
                "subject": m.subject,
                "added_at": m.added_at,
                "added_by": m.added_by,
            })
        })
        .collect();
    Ok(json!({ "members": members }))
}

#[derive(Deserialize)]
struct RegisterCredentialArgs {
    /// The `attach` name a `sql_view` skill references.
    name: String,
    /// Connector kind (`postgres`|`mysql`|`sqlite`|`erpl`|`s3`|…).
    connector: String,
    /// Secret material (DSN / secret spec). Stored server-side only.
    secret: String,
}

#[derive(Deserialize)]
struct CredentialNameArgs {
    name: String,
}

async fn tool_register_credential(
    indexer: &Indexer,
    created_by: &str,
    args: Value,
) -> Result<Value, JsonRpcError> {
    let a: RegisterCredentialArgs = serde_json::from_value(args)
        .map_err(|e| JsonRpcError::invalid_params(format!("register_credential: {e}")))?;
    if a.name.is_empty() || a.connector.is_empty() || a.secret.is_empty() {
        return Err(JsonRpcError::invalid_params(
            "name, connector, and secret are all required".to_owned(),
        ));
    }
    indexer
        .register_credential(&a.name, &a.connector, &a.secret, Some(created_by))
        .await
        .map_err(|e| JsonRpcError::internal(format!("register_credential: {e}")))?;
    // Never echo the secret back.
    Ok(json!({ "ok": true, "name": a.name }))
}

async fn tool_list_credentials(indexer: &Indexer) -> Result<Value, JsonRpcError> {
    let creds = indexer
        .list_credentials()
        .await
        .map_err(|e| JsonRpcError::internal(format!("list_credentials: {e}")))?;
    // The secret is intentionally absent from this view (REQ-SQL-05).
    let creds: Vec<Value> = creds
        .into_iter()
        .map(|c| {
            json!({
                "name": c.name,
                "connector": c.connector,
                "created_at": c.created_at,
                "created_by": c.created_by,
            })
        })
        .collect();
    Ok(json!({ "credentials": creds }))
}

async fn tool_delete_credential(indexer: &Indexer, args: Value) -> Result<Value, JsonRpcError> {
    let a: CredentialNameArgs = serde_json::from_value(args)
        .map_err(|e| JsonRpcError::invalid_params(format!("delete_credential: {e}")))?;
    indexer
        .delete_credential(&a.name)
        .await
        .map_err(|e| JsonRpcError::internal(format!("delete_credential: {e}")))?;
    Ok(json!({ "ok": true }))
}

async fn tool_validate_bindings(indexer: &Indexer) -> Result<Value, JsonRpcError> {
    let statuses = indexer
        .validate_bindings()
        .await
        .map_err(|e| JsonRpcError::internal(format!("validate_bindings: {e}")))?;
    let degraded = statuses.iter().filter(|s| s.status != "ok").count();
    let bindings: Vec<Value> = statuses
        .into_iter()
        .map(|s| {
            json!({
                "page_id": s.page_id,
                "view": s.view,
                "status": s.status,
                "detail": s.detail,
            })
        })
        .collect();
    Ok(json!({ "ok": degraded == 0, "degraded": degraded, "bindings": bindings }))
}

#[derive(Deserialize)]
struct CreateSqlInstanceArgs {
    skill: String,
    id: String,
    #[serde(default)]
    overlay_body: Option<String>,
}

/// Admin: materialise a sql_view instance from the UI. The binding comes from
/// the skill's `backend.source` block (not the caller), so this can only
/// create instances of skills that already declare a sql_view source.
async fn tool_create_sql_instance(
    indexer: &std::sync::Arc<Indexer>,
    args: Value,
) -> Result<Value, JsonRpcError> {
    let a: CreateSqlInstanceArgs = serde_json::from_value(args)
        .map_err(|e| JsonRpcError::invalid_params(format!("create_sql_instance: {e}")))?;
    let binding = indexer
        .skill_backend(&a.skill)
        .await
        .map_err(|e| JsonRpcError::internal(format!("create_sql_instance: {e}")))?;
    let sql_view = binding.sql_view.ok_or_else(|| {
        JsonRpcError::invalid_params(format!(
            "skill `{}` does not declare a sql_view backend.source",
            a.skill
        ))
    })?;
    let body = a.overlay_body.unwrap_or_else(|| format!("# {}\n", a.id));
    let m = escurel_index::backend::SqlViewBackend::new(std::sync::Arc::clone(indexer))
        .create_instance(&a.skill, &sql_view, &a.id, &body)
        .await
        .map_err(|e| JsonRpcError::internal(format!("create_sql_instance: {e}")))?;
    Ok(json!({ "page_id": m.page_id, "view": m.view }))
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

/// Reject an admin tool whose `tenant_id` argument targets a tenant
/// other than the one this single-tenant gateway is bound to. An empty
/// arg means "this gateway's tenant" and always passes. Without this
/// guard a `--tenant other` request silently operates on / reports the
/// wrong tenant (the gRPC admin surface enforced the same match).
fn ensure_tenant_matches(indexer: &Indexer, tenant_id: &str) -> Result<(), JsonRpcError> {
    if !tenant_id.is_empty() && tenant_id != indexer.tenant() {
        return Err(JsonRpcError::failed_precondition(format!(
            "tenant `{tenant_id}` does not match this gateway's tenant `{}`",
            indexer.tenant()
        )));
    }
    Ok(())
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
    tenant_id: String,
    #[serde(default)]
    source_url: String,
}

async fn tool_attach_external(state: &AppState, args: Value) -> Result<Value, JsonRpcError> {
    let a: AttachExternalArgs = serde_json::from_value(args)
        .map_err(|e| JsonRpcError::invalid_params(format!("attach_external: {e}")))?;
    let indexer = admin_indexer(state)?.clone();
    ensure_tenant_matches(&indexer, &a.tenant_id)?;
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
    // Derive a safe catalog alias from the source — the caller does
    // not choose it (matches the gRPC contract; the returned
    // `source_id` is this derived alias, not the tenant).
    let alias = derive_attach_alias(&a.source_url).ok_or_else(|| {
        JsonRpcError::invalid_params("could not derive a catalog alias from source_url".to_owned())
    })?;
    indexer
        .attach_external(&alias, &a.source_url)
        .await
        .map_err(|e| JsonRpcError::internal(format!("attach_external: {e}")))?;
    to_value(AttachExternalResponse { source_id: alias })
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
    // A wrong `tenant_id` must not silently rebuild this gateway's
    // (only) tenant.
    ensure_tenant_matches(&indexer, &a.tenant_id)?;
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
                "admin_webhook_deliveries",
                "Admin: recent outbound capture-webhook delivery outcomes \
                 (newest first) — event_id, ok, http_status, error. \
                 `configured: false` when no ESCUREL_WEBHOOK_URL is set.",
                json!({
                    "type": "object",
                    "properties": {
                        "limit": { "type": "integer", "minimum": 1, "maximum": 200, "default": 100 }
                    }
                }),
            ),
            tool_entry(
                "admin_index_query",
                "Admin: read up to `limit` rows from an allow-listed index table \
                 (pages, blocks, links, crdt_ops, crdt_snapshots, \
                 chat_messages). Not arbitrary SQL.",
                json!({
                    "type": "object",
                    "required": ["table"],
                    "properties": {
                        "table": {
                            "type": "string",
                            "enum": ["pages", "blocks", "links",
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
            tool_entry(
                "add_group_member",
                "Admin: add a principal `subject` to a custom RBAC group \
                 `group_id`. Idempotent. Membership is the source of truth \
                 for groups escurel manages; reserved names \
                 (public/owner/admin) are resolved structurally and ignored \
                 if stored.",
                json!({
                    "type": "object",
                    "required": ["group_id", "subject"],
                    "properties": {
                        "group_id": { "type": "string", "description": "The group name." },
                        "subject": { "type": "string", "description": "The principal `sub`." }
                    }
                }),
            ),
            tool_entry(
                "remove_group_member",
                "Admin: remove a principal `subject` from a custom RBAC \
                 group `group_id`. No-op when the row is absent.",
                json!({
                    "type": "object",
                    "required": ["group_id", "subject"],
                    "properties": {
                        "group_id": { "type": "string" },
                        "subject": { "type": "string" }
                    }
                }),
            ),
            tool_entry(
                "list_group_members",
                "Admin: list the members of a custom RBAC group, with \
                 grant time + granting admin (audit).",
                json!({
                    "type": "object",
                    "required": ["group_id"],
                    "properties": {
                        "group_id": { "type": "string" }
                    }
                }),
            ),
            tool_entry(
                "register_credential",
                "Admin: register (or replace) a named external-source \
                 credential a sql_view skill references via \
                 `backend.source.attach`. The secret is stored server-side \
                 and NEVER in the markdown corpus (REQ-SQL-05).",
                json!({
                    "type": "object",
                    "required": ["name", "connector", "secret"],
                    "properties": {
                        "name": { "type": "string", "description": "The `attach` name skills reference." },
                        "connector": { "type": "string", "description": "postgres|mysql|sqlite|erpl|s3|…" },
                        "secret": { "type": "string", "description": "DSN / secret material (server-side only)." }
                    }
                }),
            ),
            tool_entry(
                "list_credentials",
                "Admin: list registered external-source credentials WITHOUT \
                 their secrets (name, connector, registration audit).",
                json!({ "type": "object", "properties": {} }),
            ),
            tool_entry(
                "delete_credential",
                "Admin: remove a registered external-source credential by \
                 name. No-op when absent.",
                json!({
                    "type": "object",
                    "required": ["name"],
                    "properties": { "name": { "type": "string" } }
                }),
            ),
            tool_entry(
                "validate_bindings",
                "Admin: re-probe every SQL-view binding and report schema \
                 drift (binding_degraded) or unreachable sources \
                 (backend_unavailable). Reconciles views ⟂ backend_refs.",
                json!({ "type": "object", "properties": {} }),
            ),
            tool_entry(
                "create_sql_instance",
                "Admin: materialise a sql_view instance — the binding comes \
                 from the skill's backend.source block (read-only view + \
                 overlay page).",
                json!({
                    "type": "object",
                    "required": ["skill", "id"],
                    "properties": {
                        "skill": { "type": "string", "description": "A skill declaring backend.kind=sql_view." },
                        "id": { "type": "string", "description": "New instance id." },
                        "overlay_body": { "type": "string", "description": "Optional overlay markdown body." }
                    }
                }),
            ),
            // Admin tenant-lifecycle + operator tools. All require an
            // admin-role bearer (JSON-RPC -32001 otherwise) and a
            // `tenant_id` naming this single-tenant gateway's tenant
            // (-32002 on a mismatch).
            tool_entry(
                "tenant_create",
                "Admin: provision a tenant (directory + DuckDB file).",
                json!({
                    "type": "object",
                    "required": ["tenant_id"],
                    "properties": {
                        "tenant_id": { "type": "string" },
                        "display_name": { "type": "string" }
                    }
                }),
            ),
            tool_entry(
                "tenant_list",
                "Admin: list all tenants in the tenant store.",
                json!({ "type": "object", "properties": {} }),
            ),
            tool_entry(
                "tenant_get",
                "Admin: fetch one tenant's spec.",
                json!({
                    "type": "object",
                    "required": ["tenant_id"],
                    "properties": { "tenant_id": { "type": "string" } }
                }),
            ),
            tool_entry(
                "tenant_update",
                "Admin: update a tenant's spec (e.g. display name).",
                json!({
                    "type": "object",
                    "required": ["tenant_id"],
                    "properties": {
                        "tenant_id": { "type": "string" },
                        "display_name": { "type": "string" }
                    }
                }),
            ),
            tool_entry(
                "tenant_delete",
                "Admin: delete a tenant and its on-disk state.",
                json!({
                    "type": "object",
                    "required": ["tenant_id"],
                    "properties": { "tenant_id": { "type": "string" } }
                }),
            ),
            tool_entry(
                "tenant_export",
                "Admin: export a tenant's canonical markdown as a base64 \
                 tar+gz blob (`tarball_b64` + `bytes`).",
                json!({
                    "type": "object",
                    "required": ["tenant_id"],
                    "properties": { "tenant_id": { "type": "string" } }
                }),
            ),
            tool_entry(
                "tenant_import",
                "Admin: import a tenant's markdown from a base64 tar+gz blob \
                 into an existing tenant; returns `bytes_imported`.",
                json!({
                    "type": "object",
                    "required": ["tenant_id", "tarball_b64"],
                    "properties": {
                        "tenant_id": { "type": "string" },
                        "tarball_b64": { "type": "string" }
                    }
                }),
            ),
            tool_entry(
                "rebuild",
                "Admin: rebuild the tenant's index from canonical markdown; \
                 returns the final `{done, total}` page counts.",
                json!({
                    "type": "object",
                    "properties": {
                        "tenant_id": { "type": "string", "description": "Must match this gateway's tenant." }
                    }
                }),
            ),
            tool_entry(
                "compact_lanes",
                "Admin: compact the tenant's CRDT op lanes; returns \
                 `{ops_compacted, bytes_reclaimed}`.",
                json!({
                    "type": "object",
                    "required": ["tenant_id"],
                    "properties": { "tenant_id": { "type": "string" } }
                }),
            ),
            tool_entry(
                "attach_external",
                "Admin: attach an external read-only DuckDB source; the \
                 catalog alias is derived from `source_url` and returned as \
                 `source_id`.",
                json!({
                    "type": "object",
                    "required": ["source_url"],
                    "properties": {
                        "tenant_id": { "type": "string", "description": "Must match this gateway's tenant." },
                        "source_url": { "type": "string" }
                    }
                }),
            ),
            tool_entry(
                "embedding_reload",
                "Admin: hot-reload the embedding model from the captured \
                 config; returns the new `model_revision`.",
                json!({ "type": "object", "properties": {} }),
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
