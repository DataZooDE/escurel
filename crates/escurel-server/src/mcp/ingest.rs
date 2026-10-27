//! The `/ingest` REST surface: upload a blob, materialise it as a
//! `document`-backend instance, and dispatch the extraction worker.
//!
//! Split out of `mcp.rs` (R1 of `docs/notes/complexity-reduction-plan.md`).
//! This is REST rather than JSON-RPC and shares only the application state
//! with the tool surface, so co-locating it with tool dispatch was mixing two
//! protocols in one file.

use super::*;
use std::hash::{Hash, Hasher};
use std::sync::OnceLock;

// Bound lock memory while serialising materialisation of the same document.
static DOCUMENT_INGEST_LOCKS: OnceLock<[tokio::sync::Mutex<()>; 256]> = OnceLock::new();

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
    /// Optional caller-supplied idempotency key (escurel#382): fed to
    /// `capture_event`'s dedup, so a redelivery neither mints a second
    /// inbox event nor re-runs the extraction worker. Absent = server
    /// mints a fresh ULID per request (the legacy behaviour).
    #[serde(default)]
    event_id: Option<String>,
}

/// The authenticated caller of an ingest request — the bits needed to enforce
/// create-ACL on an explicit target skill (mirrors the tools path).
struct IngestCaller {
    subject: String,
    /// RBAC token groups (admin role value already stripped).
    groups: Vec<String>,
    is_admin: bool,
    /// The token's delegation / run / narrowing claims. An upload is made by whoever presented the
    /// bearer, and a MACHINE (`run_id` / `act` / `skill`) is gated like it is on `update_page`.
    actor: Option<String>,
    run_id: Option<String>,
    root_event_id: Option<String>,
    agent_skill: Option<String>,
}

/// Auth (REQ-NF-07) + per-tenant Writes rate-limit gate shared by `/ingest`
/// and `/ingest/upload`. Returns a cloned indexer handle + the caller (subject,
/// groups, admin) for downstream ACL checks, or an error response.
async fn ingest_gate(
    state: &crate::server::AppState,
    headers: &HeaderMap,
) -> Result<(std::sync::Arc<Indexer>, IngestCaller), axum::response::Response> {
    let auth_ctx = crate::auth_gate::authenticate(state, headers).await?;
    if let Some(refused) = crate::role_gate::surface_refusal(state, auth_ctx.as_ref(), "/ingest") {
        return Err(refused);
    }
    // Dispatch-gate parity (API review B1): the REST intake door gives the
    // same guarantees as the MCP door. A ducklake READER owns no writes —
    // refuse before debiting quota, with the same typed vocabulary as the
    // JSON-RPC `-32004` so the client knows to retry against the writer.
    if state.reader_mode {
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({
                "error": "read_only_replica",
                "message": "this replica is a ducklake reader; retry the ingest against the writer",
            })),
        )
            .into_response());
    }
    // #247 suspend gate, mirroring `dispatch`: agent-role callers are
    // refused while an admin token can still operate (and `resume`).
    if state
        .tenant_suspended
        .load(std::sync::atomic::Ordering::Relaxed)
        && matches!(auth_ctx.as_ref().map(|c| c.role), Some(Role::Agent))
    {
        return Err((
            StatusCode::FORBIDDEN,
            Json(json!({
                "error": "tenant_suspended",
                "message": "tenant is suspended; an admin token can resume it",
            })),
        )
            .into_response());
    }
    let subject = auth_ctx
        .as_ref()
        .map(|c| c.subject.clone())
        .unwrap_or_default();
    // RBAC groups: the admin role value is stripped so it cannot act as a
    // group. No verifier (dev / on-host mode) → admin bypass below.
    let groups = crate::auth_gate::rbac_groups(state, auth_ctx.as_ref());
    let is_admin = match &auth_ctx {
        Some(c) => matches!(c.role, Role::Admin),
        None => true,
    };
    let actor = auth_ctx.as_ref().and_then(|c| c.actor.clone());
    let run_id = auth_ctx
        .as_ref()
        .and_then(|c| c.run.as_ref().map(|r| r.run_id.clone()));
    let root_event_id = auth_ctx
        .as_ref()
        .and_then(|c| c.run.as_ref().and_then(|r| r.root_event_id.clone()));
    let agent_skill = auth_ctx.as_ref().and_then(|c| c.agent_skill.clone());
    if let (Some(quota), Some(ctx)) = (state.quota.as_ref(), auth_ctx.as_ref())
        && let Err(err) = quota.try_consume(&ctx.tenant_id, Dimension::Writes)
    {
        return Err((
            StatusCode::TOO_MANY_REQUESTS,
            Json(json!({ "error": "rate_limited", "message": err.to_string() })),
        )
            .into_response());
    }
    // Capture the CURRENT indexer once per request (hot-swap seam):
    // the whole ingest runs against one consistent indexer even if a
    // snapshot adoption swaps mid-flight.
    match state.indexer.as_ref() {
        Some(h) => {
            let indexer = h.current();
            if let Some(resp) = crate::server::quarantine_refusal(&indexer) {
                return Err(resp);
            }
            Ok((
                indexer,
                IngestCaller {
                    subject,
                    groups,
                    is_admin,
                    actor,
                    run_id,
                    root_event_id,
                    agent_skill,
                },
            ))
        }
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
    body: Result<Json<IngestRequest>, axum::extract::rejection::JsonRejection>,
) -> axum::response::Response {
    let req = match body {
        Ok(Json(req)) => req,
        Err(rejection) => {
            // The same gate as a well-formed request (auth, suspension, QUARANTINE) runs BEFORE the
            // body is faulted, so a client with a malformed body still learns the tenant is waiting
            // for its migration (503) instead of a 422, and an unauthenticated one still gets a 401,
            // never the page names in the quarantine message.
            let resp = match ingest_gate(&state, &headers).await {
                Err(gate) => gate,
                Ok(_) => rejection.into_response(),
            };
            state.metrics.inc_request("/ingest", resp.status().as_u16());
            return resp;
        }
    };
    // Metrics record the OUTCOME — a refused request is not a 200.
    let resp = ingest_inner(&state, &headers, req).await;
    state.metrics.inc_request("/ingest", resp.status().as_u16());
    resp
}

async fn ingest_inner(
    state: &crate::server::AppState,
    headers: &HeaderMap,
    req: IngestRequest,
) -> axum::response::Response {
    let (indexer, caller) = match ingest_gate(state, headers).await {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    record_and_dispatch_ingest(
        &indexer,
        &state.events_tx,
        state.lake.as_ref(),
        IngestJob {
            caller_event_id: req.event_id,
            blob_id: &req.blob_id,
            content_type: &req.content_type,
            title: req.title,
            target_skill: req.skill.as_deref(),
        },
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
    /// Optional idempotency key (see [`IngestRequest::event_id`]).
    #[serde(default)]
    event_id: Option<String>,
}

pub(crate) async fn ingest_upload(
    State(state): State<crate::server::AppState>,
    headers: HeaderMap,
    Json(req): Json<IngestUploadRequest>,
) -> axum::response::Response {
    // Metrics record the OUTCOME — a refused request is not a 200.
    let resp = ingest_upload_inner(&state, &headers, req).await;
    state
        .metrics
        .inc_request("/ingest/upload", resp.status().as_u16());
    resp
}

async fn ingest_upload_inner(
    state: &crate::server::AppState,
    headers: &HeaderMap,
    req: IngestUploadRequest,
) -> axum::response::Response {
    let (indexer, caller) = match ingest_gate(state, headers).await {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    deposit_and_ingest(state, &indexer, &caller, req).await
}

/// The MCP `ingest_upload` tool: the same deposit and ingest as
/// `POST /ingest/upload`, for clients that hold only the MCP endpoint. The
/// dispatcher has already authenticated and rate-limited the caller. A refusal
/// is returned as an `ok: false` envelope carrying the REST status, so it
/// reaches the model as a tool result rather than a protocol error.
pub(super) async fn tool_ingest_upload(
    state: &crate::server::AppState,
    indexer: &std::sync::Arc<Indexer>,
    caller: AclCaller<'_>,
    args: Value,
) -> Result<Value, JsonRpcError> {
    let req: IngestUploadRequest = parse_args(args, "ingest_upload")?;
    let ingest_caller = IngestCaller {
        subject: caller.subject.to_owned(),
        groups: caller.token_groups.to_vec(),
        is_admin: caller.is_admin,
        actor: caller.actor.map(str::to_owned),
        run_id: caller.run_id.map(str::to_owned),
        root_event_id: caller.root_event_id.map(str::to_owned),
        agent_skill: caller.agent_skill.map(str::to_owned),
    };
    let response = deposit_and_ingest(state, indexer, &ingest_caller, req).await;
    let status = response.status();
    let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .map_err(|e| JsonRpcError::internal(format!("ingest_upload response body: {e}")))?;
    let payload: Value = serde_json::from_slice(&body)
        .map_err(|e| JsonRpcError::internal(format!("ingest_upload response JSON: {e}")))?;
    if status.is_success() {
        return Ok(payload);
    }
    Ok(json!({
        "ok": false,
        "issues": [{
            "severity": "error",
            "code": payload.get("error").and_then(Value::as_str).unwrap_or("ingest_upload_failed"),
            "message": payload.get("message").and_then(Value::as_str).unwrap_or("file upload failed"),
            "http_status": status.as_u16(),
        }],
    }))
}

async fn deposit_and_ingest(
    state: &crate::server::AppState,
    indexer: &std::sync::Arc<Indexer>,
    caller: &IngestCaller,
    req: IngestUploadRequest,
) -> axum::response::Response {
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
        indexer,
        &state.events_tx,
        state.lake.as_ref(),
        IngestJob {
            caller_event_id: req.event_id,
            blob_id: blob.as_str(),
            content_type: &req.content_type,
            title: req.title,
            target_skill: req.skill.as_deref(),
        },
        caller,
    )
    .await
}

/// One ingest's inputs, bundled — the shared tail takes them as a unit
/// so its signature stays readable as the surface grows.
struct IngestJob<'a> {
    /// Caller-supplied idempotency key (escurel#382); `None` = mint.
    caller_event_id: Option<String>,
    blob_id: &'a str,
    content_type: &'a str,
    title: Option<String>,
    target_skill: Option<&'a str>,
}

/// Shared tail: resolve MIME→skill (REQ-DOC-06), record the immutable ingest
/// Event (auditable), then dispatch the worker or park `no_handler_skill`.
async fn record_and_dispatch_ingest(
    indexer: &std::sync::Arc<Indexer>,
    events_tx: &tokio::sync::broadcast::Sender<std::sync::Arc<escurel_index::EventInfo>>,
    lake: Option<&escurel_index::snapshot::LakeConfig>,
    job: IngestJob<'_>,
    caller: &IngestCaller,
) -> axum::response::Response {
    let IngestJob {
        caller_event_id,
        blob_id,
        content_type,
        title,
        target_skill,
    } = job;
    let subject = caller.subject.as_str();
    // #390: an empty/whitespace idempotency key would collapse every
    // id-less ingest into ONE event (first writer wins, silent loss with
    // a success receipt). Same invariant as the MCP door: refuse it.
    if caller_event_id
        .as_deref()
        .is_some_and(|id| id.trim().is_empty())
    {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({
                "error": "invalid_event_id",
                "message": "event_id must be non-empty when supplied — it is the                             idempotency key (omit it to mint a server id)",
            })),
        )
            .into_response();
    }
    // The gateway's own id namespaces (`write-back:*`, `runner-status:*`) are not the caller's to mint:
    // the same refusal `capture_event` applies, or this door would squat a runner's status record or
    // forge a write-back witness.
    if let Some(reason) = caller_event_id
        .as_deref()
        .and_then(super::tools_write::reserved_event_id_refusal)
    {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({ "error": "invalid_event_id", "message": reason })),
        )
            .into_response();
    }
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
                        && b.document.as_ref().is_some_and(|d| {
                            escurel_index::backend::mime_claim(&d.accepts, content_type).is_some()
                        })
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
    if let Some(sk) = handler.as_deref() {
        // AUTHORIZATION: the document path reaches this skill (named or MIME-routed), and the document
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
            actor: caller.actor.as_deref(),
            run_id: caller.run_id.as_deref(),
            root_event_id: caller.root_event_id.as_deref(),
            agent_skill: caller.agent_skill.as_deref(),
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
    }
    // The autonomy gate. This door materialises an instance without `update_page`, so a machine's
    // upload (explicit skill or routed by MIME) into a skill that asks for review must not land here
    // either; a blob cannot be held as a draft, so it is refused with whom to ask.
    if let Some(sk) = handler.as_deref() {
        let machine = AclCaller {
            subject,
            is_admin: caller.is_admin,
            token_groups: &caller.groups,
            actor: caller.actor.as_deref(),
            run_id: caller.run_id.as_deref(),
            root_event_id: caller.root_event_id.as_deref(),
            agent_skill: caller.agent_skill.as_deref(),
        };
        if super::tools_write::is_machine_caller(&machine)
            && super::tools_write::skill_requires_review(indexer, sk)
                .await
                .unwrap_or(true)
        {
            return (
                StatusCode::CONFLICT,
                Json(json!({
                    "error": "review_required",
                    "message": format!(
                        "`{sk}` asks for human review (`autonomy`): an agent run cannot add \
                         documents to it on its own. Ask a person to upload it"
                    ),
                })),
            )
                .into_response();
        }
    }
    // What the handler skill declares about the intake event (`backend.event_kind`).
    let event_kind = match handler.as_deref() {
        Some(sk) => match indexer.skill_backend(sk).await {
            Ok(b) => b
                .document
                .and_then(|d| d.event_kind)
                .unwrap_or(escurel_index::EventKind::User),
            Err(e) => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({ "error": format!("skill backend: {e}") })),
                )
                    .into_response();
            }
        },
        None => escurel_index::EventKind::User,
    };
    let requested_title = title.as_deref().unwrap_or(blob_id);
    // Check the full upload identity before accepting a reused key. This also
    // retries materialisation if an earlier attempt captured its event first.
    if let Some(id) = caller_event_id.as_deref() {
        match indexer.get_event(id).await {
            Ok(Some(existing)) => {
                return existing_upload_response(
                    indexer,
                    lake,
                    &existing,
                    blob_id,
                    content_type,
                    requested_title,
                    handler.as_deref(),
                    subject,
                    title.as_deref(),
                )
                .await;
            }
            Ok(None) => {}
            Err(e) => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({ "error": format!("event lookup: {e}") })),
                )
                    .into_response();
            }
        }
    }
    let label_skill = handler.clone().unwrap_or_else(|| "ingest".to_owned());
    // GH #369: the Event consumes `title` (falling back to the blob id), but the
    // materialised instance needs it too — keep the caller's *own* title, so an
    // upload without one still falls back to the `doc-<hex12>` instance name
    // rather than being renamed after the digest string.
    let doc_title = title
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(str::to_owned);
    // capture_event returns the first writer's row when attempts race.
    let attempt_id = ulid::Ulid::new().to_string();
    let event = indexer
        .capture_event(NewEvent {
            event_id: caller_event_id,
            at: None,
            source: "ingest".to_owned(),
            mime: content_type.to_owned(),
            label_skill,
            kind: event_kind,
            instance_page_id: None,
            title: requested_title.to_owned(),
            body: String::new(),
            // Stamped like every other capture: an uploaded document is
            // un-triaged third-party content too, and without the stamp it
            // would be the one inbox entry the per-event ACL cannot scope.
            provenance: super::tools_write::stamp_captured_by(
                Some(json!({
                    "blob_id": blob_id,
                    "content_type": content_type,
                    "handler_skill": handler,
                    "by": subject,
                    "ingest_attempt_id": attempt_id,
                })),
                subject,
                // An upload is made by whoever presented the bearer; the
                // ingest path carries no delegated run.
                None,
            ),
            ..Default::default()
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
    if event
        .provenance
        .get("ingest_attempt_id")
        .and_then(Value::as_str)
        != Some(attempt_id.as_str())
    {
        return existing_upload_response(
            indexer,
            lake,
            &event,
            blob_id,
            content_type,
            requested_title,
            handler.as_deref(),
            subject,
            doc_title.as_deref(),
        )
        .await;
    }
    // Announce to in-process WS subscribers (#333), same as the MCP
    // capture path; subscriber-side ACL filtering applies.
    let _ = events_tx.send(std::sync::Arc::new(event.clone()));
    match handler {
        Some(skill) => {
            let result = run_document_ingest(
                indexer,
                lake,
                &skill,
                blob_id,
                content_type,
                &event.event_id,
                subject,
                doc_title.as_deref(),
            )
            .await;
            if result.status().is_success()
                && let Err(response) =
                    settle_ingest_event(indexer, &skill, blob_id, &event.event_id).await
            {
                return response;
            }
            result
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

#[allow(clippy::too_many_arguments)]
async fn existing_upload_response(
    indexer: &std::sync::Arc<Indexer>,
    lake: Option<&escurel_index::snapshot::LakeConfig>,
    existing: &escurel_index::EventInfo,
    blob_id: &str,
    content_type: &str,
    requested_title: &str,
    handler: Option<&str>,
    subject: &str,
    title: Option<&str>,
) -> axum::response::Response {
    let same_upload = existing.source == "ingest"
        && existing.provenance.get("blob_id").and_then(Value::as_str) == Some(blob_id)
        && existing.mime == content_type
        && existing.title == requested_title
        && existing
            .provenance
            .get(escurel_index::CAPTURED_BY_FIELD)
            .and_then(Value::as_str)
            == Some(subject)
        && existing
            .provenance
            .get("handler_skill")
            .and_then(Value::as_str)
            == handler;
    if !same_upload {
        return (
            StatusCode::CONFLICT,
            Json(json!({
                "error": "event_id_conflict",
                "message": "event_id already belongs to a different upload",
            })),
        )
            .into_response();
    }
    if let Some(skill) = handler {
        let recovered = run_document_ingest(
            indexer,
            lake,
            skill,
            blob_id,
            content_type,
            &existing.event_id,
            subject,
            title,
        )
        .await;
        if !recovered.status().is_success() {
            return recovered;
        }
        if let Err(response) =
            settle_ingest_event(indexer, skill, blob_id, &existing.event_id).await
        {
            return response;
        }
        if recovered.status() == StatusCode::ACCEPTED {
            return recovered;
        }
    }
    (
        StatusCode::OK,
        Json(json!({
            "event_id": existing.event_id,
            "blob_id": blob_id,
            "status": "duplicate",
            "page_id": handler.and_then(|skill| document_page_id(skill, blob_id)),
        })),
    )
        .into_response()
}

/// Settle the ingest event on its document when the skill declares
/// `backend.settle_event`. Runs after the document exists, so an interrupted
/// upload leaves the event open and a replay completes it (assigning twice to
/// the same page is a no-op).
async fn settle_ingest_event(
    indexer: &escurel_index::Indexer,
    skill: &str,
    blob_id: &str,
    event_id: &str,
) -> Result<(), axum::response::Response> {
    let declared = indexer
        .skill_backend(skill)
        .await
        .ok()
        .and_then(|b| b.document)
        .is_some_and(|d| d.settle_event);
    if !declared {
        return Ok(());
    }
    let Some(page_id) = document_page_id(skill, blob_id) else {
        return Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": "invalid blob id for event settlement" })),
        )
            .into_response());
    };
    indexer.assign_event(event_id, &page_id).await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": format!("settle ingest event: {e}") })),
        )
            .into_response()
    })
}

/// Media types that name one representation: a replay of the same bytes under
/// either spelling is the same document. JSON is only aliased with plain text
/// when the bytes really parse as JSON.
fn mime_alias(original: &str, requested: &str, bytes: &[u8]) -> bool {
    let media = |m: &str| m.split(';').next().unwrap_or(m).trim().to_ascii_lowercase();
    let (a, b) = (media(original), media(requested));
    if a == b {
        return true;
    }
    matches!(
        (a.as_str(), b.as_str()),
        ("text/xml", "application/xml") | ("application/xml", "text/xml")
    ) || (matches!(
        (a.as_str(), b.as_str()),
        ("text/plain", "application/json") | ("application/json", "text/plain")
    ) && serde_json::from_slice::<Value>(bytes).is_ok())
}

fn document_page_id(skill: &str, blob_id_str: &str) -> Option<String> {
    let blob_id = escurel_storage::BlobId::parse(blob_id_str)?;
    let instance_id = format!("doc-{}", &blob_id.hex()[..12.min(blob_id.hex().len())]);
    Some(format!("markdown/instances/{skill}/{instance_id}.md"))
}

/// Run the deterministic ingest worker inline: extract+chunk off the write
/// lock, materialise under a brief lock. v1 uses the born-digital text
/// processor (kreuzberg PDF/DOCX is gated on the MSRV decision).
#[allow(clippy::too_many_arguments)]
async fn run_document_ingest(
    indexer: &std::sync::Arc<Indexer>,
    lake: Option<&escurel_index::snapshot::LakeConfig>,
    skill: &str,
    blob_id_str: &str,
    content_type: &str,
    event_id: &str,
    subject: &str,
    title: Option<&str>,
) -> axum::response::Response {
    use escurel_index::backend::{
        ChunkConfig, DeterministicProcessor, DocumentIngestWorker, ExtractConfig, Extractor,
        IngestOutcome, OcrPolicy, PlainTextExtractor, RetainedExtractor, RetainedMediaExtractor,
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
    let page_id = document_page_id(skill, blob_id_str)
        .expect("validated blob ID must produce a document page ID");
    let mut lock_key = std::collections::hash_map::DefaultHasher::new();
    indexer.tenant().hash(&mut lock_key);
    skill.hash(&mut lock_key);
    blob_id_str.hash(&mut lock_key);
    let locks =
        DOCUMENT_INGEST_LOCKS.get_or_init(|| std::array::from_fn(|_| tokio::sync::Mutex::new(())));
    let _ingest_guard = locks[(lock_key.finish() as usize) % locks.len()]
        .lock()
        .await;
    let mut retry_overlay: Option<String> = None;
    'existing_page: {
        match indexer.read_page_markdown(&page_id).await {
            Ok(Some(existing)) => {
                let parsed = escurel_md::parse(&existing).ok();
                let backend = parsed
                    .as_ref()
                    .and_then(|page| page.frontmatter.fields.get("backend_ref"))
                    .and_then(|value| value.as_mapping());
                let field = |name| {
                    backend
                        .and_then(|map| map.get(name))
                        .and_then(|value| value.as_str())
                };
                if field("kind") != Some("document") || field("blob_id") != Some(blob_id_str) {
                    return (
                        StatusCode::CONFLICT,
                        Json(json!({"error":"document_blob_conflict","page_id":page_id})),
                    )
                        .into_response();
                }
                if field("status") == Some("extraction_failed") {
                    // A failed extraction keeps its original in the inbox. A repeat
                    // upload reprocesses those verified bytes (an extractor may
                    // have changed), only under the original MIME, and keeps the
                    // artifact's first title and owner.
                    if field("content_type") != Some(content_type) {
                        return (
                            StatusCode::CONFLICT,
                            Json(json!({"error":"document_mime_conflict","page_id":page_id})),
                        )
                            .into_response();
                    }
                    let inbox_bytes = match indexer.read_inbox_blob(&blob_id).await {
                        Ok(bytes) => bytes,
                        Err(error) => {
                            return (
                                StatusCode::SERVICE_UNAVAILABLE,
                                Json(json!({"error":"document_inbox_unavailable",
                                "message":error.to_string()})),
                            )
                                .into_response();
                        }
                    };
                    use sha2::Digest;
                    if format!("{:x}", sha2::Sha256::digest(&inbox_bytes)) != blob_id.hex() {
                        return (
                            StatusCode::SERVICE_UNAVAILABLE,
                            Json(json!({"error":"document_blob_corrupt","page_id":page_id})),
                        )
                            .into_response();
                    }
                    retry_overlay = Some(existing);
                    break 'existing_page;
                }
                if field("status") != Some("ok") {
                    return (
                        StatusCode::CONFLICT,
                        Json(json!({"error":"document_overlay_incomplete","page_id":page_id})),
                    )
                        .into_response();
                }
                let mut recovered_blob = false;
                let canonical_bytes = match indexer.read_blob(&blob_id).await {
                    Ok(bytes) => bytes,
                    Err(IndexerError::Store(StoreError::NotFound(_))) => {
                        let promoted = indexer.promote_blob(&blob_id).await;
                        match indexer.read_blob(&blob_id).await {
                            Ok(bytes) => {
                                recovered_blob = true;
                                bytes
                            }
                            Err(error) => {
                                return (StatusCode::SERVICE_UNAVAILABLE, Json(json!({
                                "error":"document_blob_recovery_failed",
                                "message":format!("promotion: {promoted:?}; canonical read: {error}")
                            }))).into_response();
                            }
                        }
                    }
                    Err(error) => {
                        return (
                            StatusCode::SERVICE_UNAVAILABLE,
                            Json(json!({
                                "error":"document_blob_unavailable","message":error.to_string()
                            })),
                        )
                            .into_response();
                    }
                };
                use sha2::Digest;
                if format!("{:x}", sha2::Sha256::digest(&canonical_bytes)) != blob_id.hex() {
                    return (
                        StatusCode::SERVICE_UNAVAILABLE,
                        Json(json!({"error":"document_blob_corrupt","page_id":page_id})),
                    )
                        .into_response();
                }
                let original_mime = field("content_type").unwrap_or_default();
                if original_mime != content_type
                    && !mime_alias(original_mime, content_type, &canonical_bytes)
                {
                    return (
                        StatusCode::CONFLICT,
                        Json(json!({"error":"document_mime_conflict","page_id":page_id})),
                    )
                        .into_response();
                }
                if recovered_blob
                    && let Some(cfg) = lake
                    && let Err(error) = indexer.publish_page_sync(cfg, &page_id).await
                {
                    return (
                        StatusCode::SERVICE_UNAVAILABLE,
                        Json(json!({
                            "error":"document_recovery_publish_failed","message":error.to_string()
                        })),
                    )
                        .into_response();
                }
                return (
                StatusCode::OK,
                Json(json!({
                    "event_id":event_id,"blob_id":blob_id_str,"handler_skill":skill,
                    "status":"existing_document","page_id":page_id,"recovered_blob":recovered_blob
                })),
            )
                .into_response();
            }
            Ok(None) => {}
            Err(error) => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({"error":format!("existing document lookup: {error}")})),
                )
                    .into_response();
            }
        }
    }
    let media_type = content_type
        .split(';')
        .next()
        .unwrap_or(content_type)
        .trim();
    let retain = indexer
        .skill_backend(skill)
        .await
        .ok()
        .and_then(|b| b.document)
        .map(|d| d.retain)
        .unwrap_or_default();
    let extractor: std::sync::Arc<dyn Extractor> =
        if escurel_index::backend::mime_claim(&retain, media_type).is_some() {
            std::sync::Arc::new(RetainedExtractor)
        } else if media_type.starts_with("text/")
            || matches!(media_type, "application/xml" | "application/json")
        {
            std::sync::Arc::new(PlainTextExtractor)
        } else if media_type.starts_with("audio/") {
            // CR-4 (GH #356): audio is RETAINED, not extracted. Escurel does not
            // transcribe — the consumer files the transcript as its own content —
            // so the recording materialises as a zero-chunk instance whose value
            // is its identity, its links and its retained bytes. Routing it to a
            // text/kreuzberg extractor instead would mark perfectly good evidence
            // `extraction_failed`.
            std::sync::Arc::new(RetainedMediaExtractor)
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
    )
    .with_contextualize(indexer.contextualize_mode());
    // Variant B (#216): attach the LLM contextualizer when built with the
    // `contextualize-llm` feature, the mode is `llm`, and an endpoint is set.
    // Otherwise `Llm` mode degrades to structural in the pure ingest path.
    #[cfg(feature = "contextualize-llm")]
    let worker = {
        let endpoint = std::env::var("ESCUREL_CONTEXTUALIZE_LLM_ENDPOINT").unwrap_or_default();
        let key = std::env::var("ESCUREL_CONTEXTUALIZE_LLM_API_KEY").unwrap_or_default();
        if indexer.contextualize_mode() == escurel_index::backend::document::ContextualizeMode::Llm
            && !endpoint.is_empty()
        {
            worker.with_llm_contextualizer(std::sync::Arc::new(
                escurel_index::backend::contextualize_llm::LlmContextualizer::new(endpoint, key),
            ))
        } else {
            worker
        }
    };

    // Stamp the uploader as the instance owner so owner-scoped document skills
    // work: a personal skill (`read: [owner]`) stays visible only to its
    // uploader, and a group-shared skill (`read: [owner, <group>]`) is owned by
    // the uploader but readable by the group. Resolved from the skill's
    // `owner_field`; skipped for skills without one (or an anonymous caller).
    //
    // The caller's `title` (GH #369) rides in the same map: `document_overlay`
    // uses a `title`/`titel` key for the `# <title>` heading AND writes every
    // extra key as instance frontmatter, so one insert makes the upload both
    // readable and findable — `search` and `list_instances` project
    // frontmatter, and a heading-only title would not reach them.
    let retry_page = retry_overlay
        .as_deref()
        .and_then(|overlay| escurel_md::parse(overlay).ok());
    let retry_field = |name: &str| {
        retry_page
            .as_ref()
            .and_then(|page| page.frontmatter.fields.get(name))
            .and_then(|value| value.as_str())
    };
    let mut extra_map = serde_json::Map::new();
    let title = if retry_overlay.is_some() {
        retry_field("title")
    } else {
        title
    };
    if let Some(t) = title {
        extra_map.insert("title".to_owned(), json!(t));
    }
    let owner_field = match indexer.list_skills().await {
        Ok(skills) => skills
            .into_iter()
            .find(|s| s.id == skill)
            .and_then(|s| s.owner_field)
            .filter(|_| !subject.is_empty()),
        Err(_) => None,
    };
    // Inserted last so the owner stamp wins: it is an authorization fact and a
    // caller-supplied title must never be able to displace it.
    if let Some(field) = owner_field {
        let owner = if retry_overlay.is_some() {
            retry_field(&field)
        } else if subject.is_empty() {
            None
        } else {
            Some(subject)
        };
        if let Some(owner) = owner {
            extra_map.insert(field, json!(owner));
        }
    }
    let extra = if extra_map.is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::Value::Object(extra_map)
    };

    match worker
        .ingest(&blob_id, content_type, skill, &instance_id, &cfg, &extra)
        .await
    {
        Ok(IngestOutcome::Materialised {
            page_id,
            chunk_count,
        }) => {
            // #567: `materialize_document`/`write_document_blocks` never
            // got #414/#415's synchronous per-write lake publish — only
            // the MCP write tools did, because at the time nothing in
            // escurel-server was found calling this path. It IS called,
            // from here: `202 Accepted` + `"status": "materialised"` is
            // exactly the same durability promise `update_page`'s
            // `ok:true` makes, so it needs the same guard before the
            // caller sees it.
            if let Some(cfg) = lake
                && let Err(e) = indexer.publish_page_sync(cfg, &page_id).await
            {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({
                        "error": format!(
                            "ingest worker: materialised locally but failed to publish \
                             durably to the lake: {e}"
                        ),
                    })),
                )
                    .into_response();
            }
            (
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
                .into_response()
        }
        Ok(IngestOutcome::ExtractionFailed { page_id, reason }) => {
            // Same guard: an `extraction_failed` overlay is still a real
            // page write (materialize_document ran with zero chunks) and
            // still gets acknowledged to the caller — #567 covers this
            // outcome too, not just a successful extraction.
            if let Some(cfg) = lake
                && let Err(e) = indexer.publish_page_sync(cfg, &page_id).await
            {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({
                        "error": format!(
                            "ingest worker: wrote the extraction_failed overlay locally \
                             but failed to publish it durably to the lake: {e}"
                        ),
                    })),
                )
                    .into_response();
            }
            (
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
                .into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": format!("ingest worker: {e}") })),
        )
            .into_response(),
    }
}

/// `GET /blob/{page_id}` — the raw-bytes download twin of
/// `POST /ingest/upload` (API review B2). Serves the retained original
/// verbatim with a real `Content-Type`, under exactly `fetch_blob`'s
/// ACL: absent, hidden and blob-less pages are one indistinguishable
/// `404`. Unlike the base64 tool there is no 25 MiB cap — the body is
/// the plain HTTP payload a browser can render or range over.
pub(crate) async fn blob_get(
    State(state): State<crate::server::AppState>,
    axum::extract::Path(page_id): axum::extract::Path<String>,
    headers: HeaderMap,
) -> axum::response::Response {
    let resp = blob_get_inner(&state, &page_id, &headers).await;
    state.metrics.inc_request("/blob", resp.status().as_u16());
    resp
}

async fn blob_get_inner(
    state: &crate::server::AppState,
    page_id: &str,
    headers: &HeaderMap,
) -> axum::response::Response {
    let auth_ctx = match crate::auth_gate::authenticate(state, headers).await {
        Ok(ctx) => ctx,
        Err(resp) => return resp,
    };
    if let Some(refused) = crate::role_gate::surface_refusal(state, auth_ctx.as_ref(), "/blob") {
        return refused;
    }
    // Suspend gate, mirroring dispatch: agent traffic stops, admin passes.
    if state
        .tenant_suspended
        .load(std::sync::atomic::Ordering::Relaxed)
        && matches!(auth_ctx.as_ref().map(|c| c.role), Some(Role::Agent))
    {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({ "error": "tenant_suspended" })),
        )
            .into_response();
    }
    let subject = auth_ctx
        .as_ref()
        .map(|c| c.subject.clone())
        .unwrap_or_default();
    let groups = crate::auth_gate::rbac_groups(state, auth_ctx.as_ref());
    let is_admin = match &auth_ctx {
        Some(c) => matches!(c.role, Role::Admin),
        None => true,
    };
    let Some(handle) = state.indexer.as_ref() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({ "error": "no indexer wired" })),
        )
            .into_response();
    };
    let indexer = handle.current();
    if let Some(resp) = crate::server::quarantine_refusal(&indexer) {
        return resp;
    }
    let caller = AclCaller {
        subject: &subject,
        is_admin,
        token_groups: &groups,
        actor: None,
        run_id: None,
        root_event_id: None,
        agent_skill: None,
    };
    match super::tools_read::resolve_readable_blob(&indexer, &caller, page_id).await {
        Ok(Some((content_type, bytes))) => (
            StatusCode::OK,
            [
                ("content-type", content_type),
                ("content-length", bytes.len().to_string()),
            ],
            bytes,
        )
            .into_response(),
        Ok(None) => (StatusCode::NOT_FOUND, Json(json!({ "error": "not_found" }))).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e })),
        )
            .into_response(),
    }
}
