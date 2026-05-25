//! gRPC mirror of the agent surface. Shares the same `AppState`
//! as the HTTP gateway so OIDC verification + quota debits behave
//! identically to `POST /mcp`.
//!
//! All nine unary Escurel RPCs are wired (`list_skills` /
//! `list_instances` / `resolve` / `expand` / `search` /
//! `neighbours` / `run_stored_query` / `validate` / `update_page`),
//! plus the `live_session` bidi stream. `validate` is the dry-run
//! authoring-feedback path: it runs the same frontmatter +
//! wikilink checks as `update_page` against the indexer but commits
//! nothing. The `EscurelAdmin` service is partly stubbed.
//!
//! Auth + quota live in [`Self::enforce`], called from every
//! handler. We don't use a tonic `Interceptor` here because tonic
//! 0.12 interceptors are synchronous and the JWKS fetch in
//! `OidcVerifier::verify` is async — `block_on` inside an
//! interceptor runs on the same tokio worker that needs to drive
//! the fetch, which deadlocks the runtime. Per-handler `.await`
//! is straightforward and keeps the auth path on the runtime's
//! own scheduler.

// `tonic::Status` is ~176 bytes by design (it carries metadata,
// trailers, optional source error). Every gRPC handler returns
// `Result<_, Status>`, so the `result_large_err` lint fires on
// the whole module. Wrapping `Status` in a `Box` here would not
// match tonic's expected return type, so we silence the lint at
// module scope.
#![allow(clippy::result_large_err)]

use std::path::Path;
use std::pin::Pin;
use std::sync::Arc;

use escurel_admin::{AdminError, TenantSpec as AdminTenantSpec, TenantStore};
use escurel_auth::{AuthContext, OidcVerifier, Role};
use escurel_crdt::Version;
use escurel_index::{Direction, Indexer, Issue, OrderDir, Severity};
use escurel_md::PageType;
use escurel_proto::v1::TenantSpec as ProtoTenantSpec;
use escurel_proto::v1::escurel_admin_server::EscurelAdmin;
use escurel_proto::v1::escurel_server::Escurel;
use escurel_proto::v1::{
    AttachExternalRequest, AttachExternalResponse, AuditRequest, AuditResponse,
    CompactLanesRequest, CompactProgress, Edge, EmbeddingReloadRequest, EmbeddingReloadResponse,
    ExpandBlock, ExpandRequest, ExpandResponse, HealthRequest, HealthResponse, InstanceInfo,
    ListInstancesRequest, ListInstancesResponse, ListSkillsRequest, ListSkillsResponse, LiveAck,
    LiveOp, NeighboursRequest, NeighboursResponse, PageRef, QuotaGetRequest, QuotaGetResponse,
    RebuildProgress, RebuildRequest, ResolveRequest, ResolveResponse, RunStoredQueryRequest,
    RunStoredQueryResponse, SearchHit, SearchRequest, SearchResponse, Skill, StoredQueryColumn,
    TenantCreateRequest, TenantCreateResponse, TenantDeleteRequest, TenantDeleteResponse,
    TenantExportChunk, TenantExportRequest, TenantGetRequest, TenantGetResponse, TenantImportChunk,
    TenantImportResponse, TenantListRequest, TenantListResponse, TenantUpdateRequest,
    TenantUpdateResponse, UpdatePageRequest, UpdatePageResponse, ValidateRequest, ValidateResponse,
    ValidationIssue, WikilinkParsed,
};
use escurel_quota::{Dimension, QuotaError};
use flate2::Compression;
use flate2::write::GzEncoder;
use futures::Stream;
use tokio::sync::mpsc;
use tokio_stream::wrappers::{ReceiverStream, UnboundedReceiverStream};
use tonic::{Request, Response, Status, Streaming};

use crate::server::AppState;

pub(crate) struct EscurelGrpc {
    state: AppState,
}

impl EscurelGrpc {
    pub(crate) fn new(state: AppState) -> Self {
        Self { state }
    }

    fn indexer(&self) -> Result<&Arc<Indexer>, Status> {
        self.state
            .indexer
            .as_ref()
            .ok_or_else(|| Status::failed_precondition("server has no indexer wired"))
    }

    /// Validate the bearer token (if a verifier is wired) and
    /// debit the per-tenant quota for `dim` (if both a quota
    /// manager and an auth context are present). Returns the
    /// verified [`AuthContext`] for downstream handlers that need
    /// the tenant id.
    async fn enforce<R>(
        &self,
        req: &Request<R>,
        dim: Option<Dimension>,
    ) -> Result<Option<AuthContext>, Status> {
        let auth_ctx = match self.state.verifier.as_ref() {
            Some(v) => Some(enforce_auth_grpc(v, req).await?),
            None => None,
        };
        if let (Some(q), Some(d), Some(ctx)) = (self.state.quota.as_ref(), dim, auth_ctx.as_ref()) {
            q.try_consume(&ctx.tenant_id, d).map_err(quota_status)?;
        }
        Ok(auth_ctx)
    }
}

#[tonic::async_trait]
impl Escurel for EscurelGrpc {
    async fn list_skills(
        &self,
        req: Request<ListSkillsRequest>,
    ) -> Result<Response<ListSkillsResponse>, Status> {
        self.enforce(&req, Some(Dimension::Queries)).await?;
        let indexer = self.indexer()?;
        let skills = indexer
            .list_skills()
            .await
            .map_err(|e| Status::internal(format!("list_skills: {e}")))?;
        let skills = skills
            .into_iter()
            .map(|s| Skill {
                id: s.id,
                description: s.description,
                required_frontmatter: s.required_frontmatter,
                optional_frontmatter: s.optional_frontmatter,
                is_event_typed: s.is_event_typed,
            })
            .collect();
        Ok(Response::new(ListSkillsResponse { skills }))
    }

    async fn list_instances(
        &self,
        req: Request<ListInstancesRequest>,
    ) -> Result<Response<ListInstancesResponse>, Status> {
        self.enforce(&req, Some(Dimension::Queries)).await?;
        let indexer = self.indexer()?;
        let r = req.into_inner();
        let order = match r.order_by_at.to_ascii_lowercase().as_str() {
            "asc" | "at asc" | "at_asc" => Some(OrderDir::Asc),
            "desc" | "at desc" | "at_desc" => Some(OrderDir::Desc),
            "" => None,
            other => {
                return Err(Status::invalid_argument(format!(
                    "order_by_at `{other}`; expected asc|desc|<empty>"
                )));
            }
        };
        let limit = if r.limit == 0 {
            None
        } else {
            Some(r.limit as usize)
        };
        let rows = indexer
            .list_instances(&r.skill, order, limit)
            .await
            .map_err(|e| Status::internal(format!("list_instances: {e}")))?;
        let instances = rows
            .into_iter()
            .map(|i| InstanceInfo {
                page_id: i.page_id,
                skill: i.skill,
                frontmatter_json: i.frontmatter.to_string(),
                at: i.at.unwrap_or_default(),
            })
            .collect();
        Ok(Response::new(ListInstancesResponse { instances }))
    }

    async fn resolve(
        &self,
        req: Request<ResolveRequest>,
    ) -> Result<Response<ResolveResponse>, Status> {
        self.enforce(&req, Some(Dimension::Queries)).await?;
        let indexer = self.indexer()?;
        let resolved = indexer
            .resolve(&req.into_inner().wikilink)
            .await
            .map_err(|e| Status::internal(format!("resolve: {e}")))?;
        let exists = resolved.exists();
        Ok(Response::new(ResolveResponse {
            parsed: Some(wikilink_to_proto(&resolved.parsed)),
            page: resolved.page.as_ref().map(page_ref_to_proto),
            exists,
        }))
    }

    async fn expand(
        &self,
        req: Request<ExpandRequest>,
    ) -> Result<Response<ExpandResponse>, Status> {
        self.enforce(&req, Some(Dimension::Queries)).await?;
        let indexer = self.indexer()?;
        let page_id = req.into_inner().page_id;
        let expanded = indexer
            .expand(&page_id)
            .await
            .map_err(|e| Status::internal(format!("expand: {e}")))?;
        match expanded {
            None => Err(Status::not_found(format!("page `{page_id}` not indexed"))),
            Some(e) => Ok(Response::new(ExpandResponse {
                page: Some(page_ref_to_proto(&e.page)),
                frontmatter_json: e.frontmatter.to_string(),
                body: e.body,
                blocks: e
                    .blocks
                    .into_iter()
                    .map(|b| ExpandBlock {
                        anchor: b.anchor,
                        content: b.content,
                    })
                    .collect(),
                wikilinks_out: e.wikilinks_out.iter().map(wikilink_to_proto).collect(),
                snapshot_version: String::new(),
            })),
        }
    }

    async fn search(
        &self,
        req: Request<SearchRequest>,
    ) -> Result<Response<SearchResponse>, Status> {
        self.enforce(&req, Some(Dimension::Queries)).await?;
        let indexer = self.indexer()?;
        let r = req.into_inner();
        let pt = match r.page_type.as_str() {
            "" | "any" => None,
            "skill" => Some(PageType::Skill),
            "instance" => Some(PageType::Instance),
            other => {
                return Err(Status::invalid_argument(format!(
                    "search page_type `{other}`; expected skill|instance|any"
                )));
            }
        };
        let skill = if r.skill.is_empty() {
            None
        } else {
            Some(r.skill.as_str())
        };
        let k = if r.k == 0 { 10 } else { r.k as usize };
        let hits = indexer
            .search(&r.q, k, pt, skill)
            .await
            .map_err(|e| Status::internal(format!("search: {e}")))?;
        let hits = hits
            .into_iter()
            .map(|h| SearchHit {
                page_id: h.page_id,
                slug: h.slug.unwrap_or_default(),
                skill: h.skill,
                page_type: match h.page_type {
                    PageType::Skill => "skill".to_owned(),
                    PageType::Instance => "instance".to_owned(),
                },
                anchor: h.anchor.unwrap_or_default(),
                snippet: h.snippet,
                score: h.score,
                frontmatter_excerpt_json: h.frontmatter_excerpt.to_string(),
            })
            .collect();
        Ok(Response::new(SearchResponse {
            hits,
            granularity: "block".to_owned(),
        }))
    }

    async fn neighbours(
        &self,
        req: Request<NeighboursRequest>,
    ) -> Result<Response<NeighboursResponse>, Status> {
        self.enforce(&req, Some(Dimension::Queries)).await?;
        let indexer = self.indexer()?;
        let r = req.into_inner();
        let dir = match r.direction.as_str() {
            "" | "both" => Direction::Both,
            "in" => Direction::In,
            "out" => Direction::Out,
            other => {
                return Err(Status::invalid_argument(format!(
                    "direction `{other}`; expected in|out|both"
                )));
            }
        };
        let link_skill = if r.link_skill.is_empty() {
            None
        } else {
            Some(r.link_skill.as_str())
        };
        let edges = indexer
            .neighbours(&r.page_id, dir, link_skill)
            .await
            .map_err(|e| Status::internal(format!("neighbours: {e}")))?;
        let edges = edges
            .into_iter()
            .map(|e| Edge {
                src_page: e.src_page,
                dst_page: e.dst_page,
                link_skill: e.link_skill,
                link_version: e.link_version.unwrap_or_default(),
                dst_anchor: e.dst_anchor.unwrap_or_default(),
            })
            .collect();
        Ok(Response::new(NeighboursResponse { edges }))
    }

    async fn run_stored_query(
        &self,
        req: Request<RunStoredQueryRequest>,
    ) -> Result<Response<RunStoredQueryResponse>, Status> {
        self.enforce(&req, Some(Dimension::Queries)).await?;
        let indexer = self.indexer()?;
        let r = req.into_inner();
        let params: serde_json::Map<String, serde_json::Value> = if r.params_json.is_empty() {
            serde_json::Map::new()
        } else {
            serde_json::from_str(&r.params_json).map_err(|e| {
                Status::invalid_argument(format!("params_json must be a JSON object: {e}"))
            })?
        };
        let out = indexer
            .run_stored_query(&r.query_id, &params)
            .await
            .map_err(|e| Status::internal(format!("run_stored_query: {e}")))?;
        let rows_json = serde_json::Value::Array(
            out.rows
                .into_iter()
                .map(serde_json::Value::Object)
                .collect(),
        )
        .to_string();
        let schema = out
            .schema
            .into_iter()
            .map(|c| StoredQueryColumn {
                name: c.name,
                type_name: c.type_name,
            })
            .collect();
        Ok(Response::new(RunStoredQueryResponse { rows_json, schema }))
    }

    async fn update_page(
        &self,
        req: Request<UpdatePageRequest>,
    ) -> Result<Response<UpdatePageResponse>, Status> {
        self.enforce(&req, Some(Dimension::Writes)).await?;
        let indexer = self.indexer()?;
        let r = req.into_inner();
        indexer
            .update_page(&r.page_id, &r.content)
            .await
            .map_err(|e| Status::internal(format!("update_page: {e}")))?;
        Ok(Response::new(UpdatePageResponse {
            ok: true,
            issues: Vec::new(),
            // Stub until the CRDT version layer lands in M4. The
            // HTTP MCP dispatcher returns the same sentinel.
            new_version: "v1".to_owned(),
        }))
    }

    async fn validate(
        &self,
        req: Request<ValidateRequest>,
    ) -> Result<Response<ValidateResponse>, Status> {
        // A dry run debits the read (`Queries`) bucket: it does the
        // same parse + skill-lookup work as a read tool and commits
        // nothing, so it should not consume the write budget.
        self.enforce(&req, Some(Dimension::Queries)).await?;
        let indexer = self.indexer()?;
        let r = req.into_inner();
        let page_id = if r.page_id.is_empty() {
            None
        } else {
            Some(r.page_id.as_str())
        };
        let issues = indexer
            .validate(page_id, &r.content)
            .await
            .map_err(|e| Status::internal(format!("validate: {e}")))?;
        // `ok` is false iff any issue is error-severity, mirroring
        // the live-write contract (an error rejects the write).
        let ok = !issues.iter().any(|i| i.severity == Severity::Error);
        let issues = issues.iter().map(issue_to_proto).collect();
        Ok(Response::new(ValidateResponse { ok, issues }))
    }

    type LiveSessionStream = Pin<Box<dyn Stream<Item = Result<LiveAck, Status>> + Send>>;
    async fn live_session(
        &self,
        req: Request<Streaming<LiveOp>>,
    ) -> Result<Response<Self::LiveSessionStream>, Status> {
        // Auth gates the stream open — same shape as unary RPCs.
        // We cannot reuse `Self::enforce` here because it borrows
        // the whole `Request<R>` across the JWKS-fetch `.await`;
        // `Streaming<LiveOp>` isn't `Sync`, so a `&Request<…>`
        // makes the handler future non-`Send`. Mirror
        // `EscurelAdminGrpc::enforce_admin` and pull the token out
        // before awaiting. The stream itself does not debit a
        // quota dimension; each op debits `Writes` inside the
        // inbound loop (mirroring the HTTP MCP `apply_op` policy
        // in `dimension_for`).
        let tenant_id = if let Some(v) = self.state.verifier.as_ref() {
            let token = extract_bearer(req.metadata())?.to_owned();
            v.verify(&token)
                .await
                .map_err(|e| Status::unauthenticated(format!("token rejected: {e}")))?
                .tenant_id
        } else {
            // Dev mode (no verifier) — mirror `mcp.rs`'s sentinel
            // so the per-tenant quota bucket is shared across
            // transports.
            "default".to_owned()
        };
        let sessions = Arc::clone(&self.state.sessions);
        let quota = self.state.quota.clone();
        let mut inbound = req.into_inner();

        // Outbound channel for `LiveAck`s. A small buffer keeps a
        // burst of acks in flight without unbounded growth — apply
        // is synchronous-per-op anyway, so the dispatcher rarely
        // gets ahead by more than one.
        let (tx, rx) = mpsc::channel::<Result<LiveAck, Status>>(8);

        tokio::spawn(async move {
            // First inbound frame is the attach.
            let Some(first) = (match inbound.message().await {
                Ok(v) => v,
                Err(status) => {
                    let _ = tx.send(Err(status)).await;
                    return;
                }
            }) else {
                // Client closed before sending anything; nothing to ack.
                return;
            };

            let session_id = first.session.clone();
            // The attach frame's `op` must be empty per the spec.
            // We do not enforce this strictly: clients that send a
            // first frame with a non-empty `op` are treated as if
            // the attach came on a separate frame and the op was
            // the first payload — but they still need to be
            // attached to a known session first, so the session
            // lookup below is the actual gate.
            let content = match sessions.current_content(&session_id).await {
                Some(c) => c,
                None => {
                    let _ = tx
                        .send(Ok(LiveAck {
                            session: session_id.clone(),
                            merged_version: String::new(),
                            content: String::new(),
                            issues: vec![ValidationIssue {
                                code: "unknown_session".to_owned(),
                                message: format!("session `{session_id}` not open"),
                                anchor: String::new(),
                            }],
                        }))
                        .await;
                    return;
                }
            };
            // Attach ack: report the *current* head + content. v1
            // always opens at v0 (see `SessionManager::open`); ops
            // applied before the attach (e.g. via HTTP `apply_op`
            // on another transport) advance the head, and we
            // surface that via the doc's content rather than
            // re-deriving the version string.
            if tx
                .send(Ok(LiveAck {
                    session: session_id.clone(),
                    merged_version: Version::from_op_count(0).as_str().to_owned(),
                    content,
                    issues: Vec::new(),
                }))
                .await
                .is_err()
            {
                return;
            }

            // If the attach frame carried an op blob, apply it.
            if !first.op.is_empty()
                && !dispatch_op(&sessions, &quota, &tenant_id, &session_id, first.op, &tx).await
            {
                return;
            }

            // Subsequent frames are ops on the attached session.
            loop {
                let next = match inbound.message().await {
                    Ok(Some(msg)) => msg,
                    Ok(None) => break,
                    Err(_) => break,
                };
                let target = if next.session.is_empty() {
                    session_id.clone()
                } else {
                    next.session.clone()
                };
                if !dispatch_op(&sessions, &quota, &tenant_id, &target, next.op, &tx).await {
                    return;
                }
            }
        });

        let stream: Self::LiveSessionStream = Box::pin(ReceiverStream::new(rx));
        Ok(Response::new(stream))
    }
}

/// Apply a single op on `session_id` and send back the resulting
/// `LiveAck`. Returns `false` when the loop should stop —
/// `unknown_session` ack delivered, quota exhausted, or the
/// outbound channel closed.
async fn dispatch_op(
    sessions: &Arc<crate::session::SessionManager>,
    quota: &Option<Arc<escurel_quota::QuotaManager>>,
    tenant_id: &str,
    session_id: &str,
    op_bytes: Vec<u8>,
    tx: &mpsc::Sender<Result<LiveAck, Status>>,
) -> bool {
    // Per-op writes debit — mirrors the HTTP `apply_op` policy in
    // `dimension_for`. Tenant id falls back to `"default"` in dev
    // mode; the quota manager's bucket map is keyed by tenant so
    // the dev fallback shares one bucket per process, matching
    // `mcp.rs`.
    if let Some(q) = quota.as_ref() {
        if let Err(err) = q.try_consume(tenant_id, Dimension::Writes) {
            let _ = tx.send(Err(quota_status(err))).await;
            return false;
        }
    }
    match sessions
        .apply(session_id, escurel_crdt::Op::new(op_bytes))
        .await
    {
        Ok(version) => {
            let content = sessions
                .current_content(session_id)
                .await
                .unwrap_or_default();
            let ack = LiveAck {
                session: session_id.to_owned(),
                merged_version: version.as_str().to_owned(),
                content,
                issues: Vec::new(),
            };
            tx.send(Ok(ack)).await.is_ok()
        }
        Err(crate::session::SessionError::UnknownSession(_)) => {
            let _ = tx
                .send(Ok(LiveAck {
                    session: session_id.to_owned(),
                    merged_version: String::new(),
                    content: String::new(),
                    issues: vec![ValidationIssue {
                        code: "unknown_session".to_owned(),
                        message: format!("session `{session_id}` not open"),
                        anchor: String::new(),
                    }],
                }))
                .await;
            false
        }
        Err(e) => {
            let _ = tx
                .send(Err(Status::internal(format!("live_session: {e}"))))
                .await;
            false
        }
    }
}

// --- auth + quota helpers -------------------------------------------

async fn enforce_auth_grpc<R>(
    verifier: &OidcVerifier,
    req: &Request<R>,
) -> Result<AuthContext, Status> {
    // Pull the token out of the metadata *before* the await so we
    // don't borrow the request across it. Request bodies like
    // `Streaming<T>` aren't `Sync`, which would otherwise make the
    // returned future non-Send.
    let token = extract_bearer(req.metadata())?.to_owned();
    verifier
        .verify(&token)
        .await
        .map_err(|e| Status::unauthenticated(format!("token rejected: {e}")))
}

fn extract_bearer(md: &tonic::metadata::MetadataMap) -> Result<&str, Status> {
    let raw = md
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| Status::unauthenticated("missing authorization metadata"))?;
    strip_bearer(raw)
        .ok_or_else(|| Status::unauthenticated("authorization must start with `Bearer `"))
}

fn strip_bearer(raw: &str) -> Option<&str> {
    raw.strip_prefix("Bearer ")
        .or_else(|| raw.strip_prefix("bearer "))
        .map(str::trim)
}

fn quota_status(err: QuotaError) -> Status {
    let retry = err.retry_after_ms();
    let mut status = Status::resource_exhausted(format!("quota exhausted: {err}"));
    if let Ok(v) = retry.to_string().parse() {
        status.metadata_mut().insert("retry-after-ms", v);
    }
    status
}

// --- proto <-> domain helpers ---------------------------------------

fn page_ref_to_proto(p: &escurel_index::PageRef) -> PageRef {
    PageRef {
        page_id: p.page_id.clone(),
        slug: p.slug.clone().unwrap_or_default(),
        skill: p.skill.clone(),
        page_type: match p.page_type {
            PageType::Skill => "skill".to_owned(),
            PageType::Instance => "instance".to_owned(),
        },
    }
}

fn wikilink_to_proto(w: &escurel_md::wikilink::WikilinkParsed) -> WikilinkParsed {
    WikilinkParsed {
        skill: w.skill.clone().unwrap_or_default(),
        id: w.id.clone().unwrap_or_default(),
        anchor: w.anchor.clone().unwrap_or_default(),
        version: w.version.clone().unwrap_or_default(),
        alias: w.alias.clone().unwrap_or_default(),
    }
}

/// Map a domain [`Issue`] to the proto [`ValidationIssue`]. The
/// proto carries `code` / `message` / `anchor`; we fold the
/// domain's richer `severity` + `location` into the wire by
/// prefixing the message with the severity (so a gRPC client still
/// sees error-vs-warning) and routing `location` into the `anchor`
/// field, which already names "where" for the live-CRDT acks.
fn issue_to_proto(issue: &Issue) -> ValidationIssue {
    ValidationIssue {
        code: issue.code.clone(),
        message: format!("[{}] {}", issue.severity.as_str(), issue.message),
        anchor: issue.location.clone(),
    }
}

fn proto_to_admin_spec(spec: Option<ProtoTenantSpec>) -> Result<AdminTenantSpec, Status> {
    let s = spec.ok_or_else(|| Status::invalid_argument("missing TenantSpec"))?;
    Ok(AdminTenantSpec {
        tenant_id: s.tenant_id,
        display_name: s.display_name,
    })
}

fn admin_to_proto_spec(s: &AdminTenantSpec) -> ProtoTenantSpec {
    ProtoTenantSpec {
        tenant_id: s.tenant_id.clone(),
        display_name: s.display_name.clone(),
    }
}

/// Translate an [`AdminError`] into a `tonic::Status` for the
/// admin RPCs. The error→code mapping is the same shape the spec
/// describes for admin endpoints in `docs/spec/protocol.md
/// §Admin surface`: invalid input → `invalid_argument`,
/// duplicate create → `already_exists`, missing tenant on update
/// → `not_found`, real I/O / corruption failures → `internal`.
fn admin_status(err: AdminError) -> Status {
    match err {
        AdminError::InvalidTenantId(_) => Status::invalid_argument(err.to_string()),
        AdminError::AlreadyExists(_) => Status::already_exists(err.to_string()),
        AdminError::Io { ref source, .. } if source.kind() == std::io::ErrorKind::NotFound => {
            Status::not_found(err.to_string())
        }
        AdminError::Io { .. }
        | AdminError::Duckdb { .. }
        | AdminError::Migration { .. }
        | AdminError::Malformed { .. } => Status::internal(err.to_string()),
    }
}

// ====================================================================
// EscurelAdmin
// ====================================================================
//
// Stubs for the admin surface. `Health` returns the configured
// version and works without auth (substrate liveness probe). Every
// other RPC requires `Admin` role on the bearer JWT and returns
// `Status::unimplemented` until the M4 admin endpoints land.

pub(crate) struct EscurelAdminGrpc {
    state: AppState,
}

impl EscurelAdminGrpc {
    pub(crate) fn new(state: AppState) -> Self {
        Self { state }
    }

    /// Enforce admin-role auth. Returns `Status::unauthenticated`
    /// on missing/invalid token; `Status::permission_denied` when
    /// the token verified but the caller is only `Role::Agent`.
    ///
    /// Takes only the `MetadataMap` — never the whole `Request<R>`
    /// — because some admin RPCs accept `Streaming<T>` request
    /// bodies, which aren't `Sync`, so borrowing the whole request
    /// across the JWKS-fetch `.await` makes the handler future
    /// non-`Send`.
    async fn enforce_admin(
        &self,
        md: &tonic::metadata::MetadataMap,
    ) -> Result<AuthContext, Status> {
        let verifier = self
            .state
            .verifier
            .as_ref()
            .ok_or_else(|| Status::failed_precondition("server has no verifier wired"))?;
        let token = extract_bearer(md)?.to_owned();
        let ctx = verifier
            .verify(&token)
            .await
            .map_err(|e| Status::unauthenticated(format!("token rejected: {e}")))?;
        if !matches!(ctx.role, Role::Admin) {
            return Err(Status::permission_denied(
                "admin role required (token has agent role only)",
            ));
        }
        Ok(ctx)
    }

    fn tenant_store(&self) -> Result<&Arc<dyn TenantStore>, Status> {
        self.state
            .tenant_store
            .as_ref()
            .ok_or_else(|| Status::failed_precondition("server has no tenant_store wired"))
    }
}

type StreamOf<T> = Pin<Box<dyn Stream<Item = Result<T, Status>> + Send>>;

#[tonic::async_trait]
impl EscurelAdmin for EscurelAdminGrpc {
    async fn health(
        &self,
        _req: Request<HealthRequest>,
    ) -> Result<Response<HealthResponse>, Status> {
        // Health is dependency-free auth-wise: substrate liveness
        // probe must work with or without a token. The version
        // field is the source of truth for which build is running.
        Ok(Response::new(HealthResponse {
            status: "ok".to_owned(),
            version: self.state.version.clone(),
        }))
    }

    async fn tenant_create(
        &self,
        req: Request<TenantCreateRequest>,
    ) -> Result<Response<TenantCreateResponse>, Status> {
        self.enforce_admin(req.metadata()).await?;
        let store = self.tenant_store()?.clone();
        let spec = proto_to_admin_spec(req.into_inner().spec)?;
        store.create(&spec).await.map_err(admin_status)?;
        Ok(Response::new(TenantCreateResponse {
            spec: Some(admin_to_proto_spec(&spec)),
        }))
    }

    async fn tenant_list(
        &self,
        req: Request<TenantListRequest>,
    ) -> Result<Response<TenantListResponse>, Status> {
        self.enforce_admin(req.metadata()).await?;
        let store = self.tenant_store()?.clone();
        let specs = store.list().await.map_err(admin_status)?;
        Ok(Response::new(TenantListResponse {
            tenants: specs.iter().map(admin_to_proto_spec).collect(),
        }))
    }

    async fn tenant_get(
        &self,
        req: Request<TenantGetRequest>,
    ) -> Result<Response<TenantGetResponse>, Status> {
        self.enforce_admin(req.metadata()).await?;
        let store = self.tenant_store()?.clone();
        let id = req.into_inner().tenant_id;
        match store.get(&id).await.map_err(admin_status)? {
            None => Err(Status::not_found(format!("tenant `{id}` not found"))),
            Some(spec) => Ok(Response::new(TenantGetResponse {
                spec: Some(admin_to_proto_spec(&spec)),
            })),
        }
    }

    async fn tenant_update(
        &self,
        req: Request<TenantUpdateRequest>,
    ) -> Result<Response<TenantUpdateResponse>, Status> {
        self.enforce_admin(req.metadata()).await?;
        let store = self.tenant_store()?.clone();
        let spec = proto_to_admin_spec(req.into_inner().spec)?;
        store.update(&spec).await.map_err(admin_status)?;
        Ok(Response::new(TenantUpdateResponse {
            spec: Some(admin_to_proto_spec(&spec)),
        }))
    }

    async fn tenant_delete(
        &self,
        req: Request<TenantDeleteRequest>,
    ) -> Result<Response<TenantDeleteResponse>, Status> {
        self.enforce_admin(req.metadata()).await?;
        let store = self.tenant_store()?.clone();
        let id = req.into_inner().tenant_id;
        let deleted = store.delete(&id).await.map_err(admin_status)?;
        Ok(Response::new(TenantDeleteResponse { deleted }))
    }

    type TenantExportStream = StreamOf<TenantExportChunk>;
    async fn tenant_export(
        &self,
        req: Request<TenantExportRequest>,
    ) -> Result<Response<Self::TenantExportStream>, Status> {
        self.enforce_admin(req.metadata()).await?;
        let store = self.tenant_store()?.clone();
        let tenant_id = req.into_inner().tenant_id;
        // Validate before constructing on-disk paths — `tenant_dir`
        // is filesystem-direct (`root.join(tenant_id)`) and would
        // happily resolve `../other`. CRUD/import paths route
        // through `validate_tenant_id`; export bypasses them
        // (codex review on PR M4.5b).
        escurel_admin::validate_tenant_id(&tenant_id)
            .map_err(|e| Status::invalid_argument(e.to_string()))?;
        let tenant_dir = store
            .tenant_dir(&tenant_id)
            .ok_or_else(|| Status::failed_precondition("tenant store has no on-disk path"))?;
        // Spec (storage.md L320–327): only canonical markdown is
        // exported. CRDT state + DuckDB are runtime concerns, not
        // corpus artefacts.
        let markdown_dir = tenant_dir.join("markdown");
        if !tokio::fs::try_exists(&markdown_dir).await.unwrap_or(false) {
            return Err(Status::not_found(format!("tenant `{tenant_id}` not found")));
        }

        // Build the tar+gz on a blocking thread (file I/O + zlib),
        // pumping fixed-size chunks through an mpsc into a tonic
        // server-stream. Chunk size is the gRPC default-friendly
        // 64 KiB so the server doesn't have to fight max-message
        // limits for the typical tenant.
        const CHUNK: usize = 64 * 1024;
        let (tx, rx) = mpsc::channel::<Result<TenantExportChunk, Status>>(4);
        tokio::task::spawn_blocking(move || {
            let res = tar_gz_into_chunks(&markdown_dir, CHUNK, |chunk| {
                tx.blocking_send(Ok(TenantExportChunk { data: chunk }))
                    .map_err(|_| std::io::Error::other("client closed export stream"))
            });
            if let Err(e) = res {
                // Best-effort: surface the error on the stream
                // before the sender drops.
                let _ = tx.blocking_send(Err(Status::internal(format!("tenant_export: {e}"))));
            }
        });
        let stream: Self::TenantExportStream = Box::pin(ReceiverStream::new(rx));
        Ok(Response::new(stream))
    }

    async fn tenant_import(
        &self,
        req: Request<Streaming<TenantImportChunk>>,
    ) -> Result<Response<TenantImportResponse>, Status> {
        self.enforce_admin(req.metadata()).await?;
        let store = self.tenant_store()?.clone();
        let mut stream = req.into_inner();

        // Tenant id is carried on every chunk per the proto;
        // realistically the client sets it once. Pull the first
        // chunk to learn the target tenant, refuse if it doesn't
        // exist, then drain the rest into an in-memory buffer.
        let mut tenant_id: Option<String> = None;
        let mut buffer: Vec<u8> = Vec::new();
        while let Some(chunk) = stream.message().await? {
            if tenant_id.is_none() {
                if chunk.tenant_id.is_empty() {
                    return Err(Status::invalid_argument(
                        "first TenantImportChunk must carry tenant_id",
                    ));
                }
                // Validate existence up-front so a megabyte of
                // bytes don't get buffered before the rejection.
                if store
                    .get(&chunk.tenant_id)
                    .await
                    .map_err(admin_status)?
                    .is_none()
                {
                    return Err(Status::not_found(format!(
                        "tenant `{}` not found",
                        chunk.tenant_id
                    )));
                }
                tenant_id = Some(chunk.tenant_id.clone());
            }
            buffer.extend_from_slice(&chunk.data);
        }
        let tenant_id =
            tenant_id.ok_or_else(|| Status::invalid_argument("import stream had no chunks"))?;
        let tenant_dir = store
            .tenant_dir(&tenant_id)
            .ok_or_else(|| Status::failed_precondition("tenant store has no on-disk path"))?;
        let markdown_dir = tenant_dir.join("markdown");
        let bytes_imported = buffer.len() as u64;
        tokio::task::spawn_blocking(move || untar_gz_into(&buffer, &markdown_dir))
            .await
            .map_err(|e| Status::internal(format!("tenant_import join error: {e}")))?
            .map_err(|e| Status::internal(format!("tenant_import: {e}")))?;
        Ok(Response::new(TenantImportResponse { bytes_imported }))
    }

    async fn audit(&self, req: Request<AuditRequest>) -> Result<Response<AuditResponse>, Status> {
        self.enforce_admin(req.metadata()).await?;
        let indexer = self
            .state
            .indexer
            .as_ref()
            .ok_or_else(|| Status::failed_precondition("server has no indexer wired"))?;
        let r = req.into_inner();
        // Multi-tenant routing lands later — for now the audited
        // tenant must match the indexer's. Calling for any other
        // tenant is a programmer mistake, not a runtime condition,
        // so we surface it as `failed_precondition`.
        if r.tenant_id != indexer.tenant() {
            return Err(Status::failed_precondition(format!(
                "audit tenant `{}` does not match indexer tenant `{}`",
                r.tenant_id,
                indexer.tenant()
            )));
        }
        let drift = indexer
            .audit()
            .await
            .map_err(|e| Status::internal(format!("audit: {e}")))?;
        Ok(Response::new(AuditResponse {
            markdown_not_in_duckdb: drift.markdown_not_in_duckdb,
            indexed_but_no_markdown: drift.indexed_but_no_markdown,
        }))
    }

    type RebuildStream = StreamOf<RebuildProgress>;
    async fn rebuild(
        &self,
        req: Request<RebuildRequest>,
    ) -> Result<Response<Self::RebuildStream>, Status> {
        self.enforce_admin(req.metadata()).await?;
        let indexer = self
            .state
            .indexer
            .as_ref()
            .ok_or_else(|| Status::failed_precondition("server has no indexer wired"))?
            .clone();
        let r = req.into_inner();
        // Single-tenant routing today: the indexer is bound to
        // exactly one tenant at startup. Calls for any other
        // tenant are a programmer mistake, mirrored from `audit`.
        if r.tenant_id != indexer.tenant() {
            return Err(Status::failed_precondition(format!(
                "rebuild tenant `{}` does not match indexer tenant `{}`",
                r.tenant_id,
                indexer.tenant()
            )));
        }

        // Unbounded so the sync callback inside
        // `rebuild_with_progress` never has to drop chunks when
        // the consumer briefly stalls. Progress events are ~64
        // bytes each; even a 1 M-page rebuild (spec's headline
        // ceiling) is bounded to ~64 MB worst case.
        let (tx, rx) = mpsc::unbounded_channel::<Result<RebuildProgress, Status>>();
        tokio::spawn(async move {
            let progress_tx = tx.clone();
            let result = indexer
                .rebuild_with_progress(|p| {
                    // `send` on an unbounded channel only fails
                    // when the receiver is closed — i.e. the
                    // client hung up. We let the rebuild keep
                    // running, mirroring the spec's "rebuild is
                    // idempotent" stance.
                    let _ = progress_tx.send(Ok(RebuildProgress {
                        done: p.done,
                        total: p.total,
                        current_page: p.current_page.to_owned(),
                    }));
                })
                .await;
            if let Err(e) = result {
                let _ = tx.send(Err(Status::internal(format!("rebuild: {e}"))));
            }
        });
        let stream: Self::RebuildStream = Box::pin(UnboundedReceiverStream::new(rx));
        Ok(Response::new(stream))
    }

    async fn attach_external(
        &self,
        req: Request<AttachExternalRequest>,
    ) -> Result<Response<AttachExternalResponse>, Status> {
        // Auth gate stays wired so the role boundary is correct
        // on the wire; the body lands in M4.6 alongside the
        // two-stage reconciler.
        self.enforce_admin(req.metadata()).await?;
        Err(Status::unimplemented(
            "attach_external lands in M4.6 alongside the two-stage reconciler",
        ))
    }

    async fn embedding_reload(
        &self,
        req: Request<EmbeddingReloadRequest>,
    ) -> Result<Response<EmbeddingReloadResponse>, Status> {
        self.enforce_admin(req.metadata()).await?;
        // Placeholder until the embedder hot-reload path lands
        // in M5; the gate is wired so the surface matches the
        // proto.
        Ok(Response::new(EmbeddingReloadResponse {
            model_revision: "M5".to_owned(),
        }))
    }

    type CompactLanesStream = StreamOf<CompactProgress>;
    async fn compact_lanes(
        &self,
        req: Request<CompactLanesRequest>,
    ) -> Result<Response<Self::CompactLanesStream>, Status> {
        // Auth gate stays wired so the role boundary is correct;
        // the body lands post-M5 with the CRDT compaction path.
        self.enforce_admin(req.metadata()).await?;
        Err(Status::unimplemented("compaction lands post-M5"))
    }

    async fn quota_get(
        &self,
        req: Request<QuotaGetRequest>,
    ) -> Result<Response<QuotaGetResponse>, Status> {
        self.enforce_admin(req.metadata()).await?;
        let quota = self
            .state
            .quota
            .as_ref()
            .ok_or_else(|| Status::failed_precondition("server has no quota manager wired"))?;
        let snap = quota.snapshot(&req.into_inner().tenant_id);
        Ok(Response::new(QuotaGetResponse {
            queries_remaining: snap.queries_remaining,
            writes_remaining: snap.writes_remaining,
            embeds_remaining: snap.embeds_remaining,
            // Proto field name is `concurrent_sessions`; we
            // surface the *in-use* count because the cap itself
            // is reported once via config and the value that
            // actually changes hour-to-hour is occupancy.
            concurrent_sessions: snap.concurrent_sessions_in_use,
        }))
    }
}

// --- tar+gz helpers -------------------------------------------------

/// Build a gzip-framed tar of `root`'s contents and feed it through
/// `sink` in `chunk` byte slices. Runs synchronously — the caller
/// hosts it on a blocking thread.
///
/// `sink` returns `Err` when the consumer has hung up; we surface
/// that as `io::Error` to abort the tar stream.
fn tar_gz_into_chunks(
    root: &Path,
    chunk: usize,
    mut sink: impl FnMut(Vec<u8>) -> std::io::Result<()>,
) -> std::io::Result<()> {
    let writer = ChunkSink {
        chunk,
        buf: Vec::with_capacity(chunk),
        emit: &mut sink,
    };
    let gz = GzEncoder::new(writer, Compression::default());
    let mut tar = tar::Builder::new(gz);
    // Append everything under `root` as the archive root — i.e.
    // entries are stored as their path *relative to* `root`. On
    // import the consumer extracts back into another `root`, so
    // names round-trip without nesting.
    tar.append_dir_all(".", root)?;
    let gz = tar.into_inner()?;
    let mut writer = gz.finish()?;
    writer.flush_remaining()
}

/// Sink used by [`tar_gz_into_chunks`] — accumulates writes into a
/// reusable buffer and forwards full `chunk`-byte slices through
/// `emit`. The trailing fragment is emitted by `flush_remaining`.
struct ChunkSink<'a> {
    chunk: usize,
    buf: Vec<u8>,
    emit: &'a mut dyn FnMut(Vec<u8>) -> std::io::Result<()>,
}

impl ChunkSink<'_> {
    fn flush_remaining(&mut self) -> std::io::Result<()> {
        if !self.buf.is_empty() {
            let out = std::mem::take(&mut self.buf);
            (self.emit)(out)?;
        }
        Ok(())
    }
}

impl std::io::Write for ChunkSink<'_> {
    fn write(&mut self, mut data: &[u8]) -> std::io::Result<usize> {
        let total = data.len();
        while !data.is_empty() {
            let want = self.chunk - self.buf.len();
            let take = data.len().min(want);
            self.buf.extend_from_slice(&data[..take]);
            data = &data[take..];
            if self.buf.len() >= self.chunk {
                let out = std::mem::take(&mut self.buf);
                self.buf.reserve(self.chunk);
                (self.emit)(out)?;
            }
        }
        Ok(total)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Inverse of [`tar_gz_into_chunks`]: decode `bytes` as gzip+tar
/// and extract every entry under `dest`. Runs synchronously — the
/// caller hosts it on a blocking thread.
///
/// `tar::Archive::unpack` rejects entries containing `..` segments
/// by default, so the path-traversal surface mirrors the rest of
/// the admin layer (see [`escurel_admin::validate_tenant_id`] for
/// the tenant-id half of the same defence).
fn untar_gz_into(bytes: &[u8], dest: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dest)?;
    let gz = flate2::read::GzDecoder::new(bytes);
    let mut archive = tar::Archive::new(gz);
    archive.unpack(dest)
}
