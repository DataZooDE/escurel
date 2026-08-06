//! The `/ingest` REST surface: upload a blob, materialise it as a
//! `document`-backend instance, and dispatch the extraction worker.
//!
//! Split out of `mcp.rs` (R1 of `docs/notes/complexity-reduction-plan.md`).
//! This is REST rather than JSON-RPC and shares only the application state
//! with the tool surface, so co-locating it with tool dispatch was mixing two
//! protocols in one file.

use super::*;

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
        Some(v) => {
            let served = state.served_tenant.as_deref();
            match crate::auth_gate::enforce_auth(v, headers, served).await {
                Ok(c) => Some(c),
                Err(resp) => return Err(resp),
            }
        }
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
    // Capture the CURRENT indexer once per request (hot-swap seam):
    // the whole ingest runs against one consistent indexer even if a
    // snapshot adoption swaps mid-flight.
    match state.indexer.as_ref() {
        Some(h) => Ok((
            h.current(),
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
