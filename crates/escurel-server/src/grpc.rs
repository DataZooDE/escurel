//! gRPC mirror of the agent surface. Shares the same `AppState`
//! as the HTTP gateway so OIDC verification + quota debits behave
//! identically to `POST /mcp`.
//!
//! Today eight of the nine Escurel RPCs are wired
//! (`list_skills` / `list_instances` / `resolve` / `expand` /
//! `search` / `neighbours` / `run_stored_query` / `update_page`).
//! `validate` returns `Unimplemented` until the standalone
//! validator lands; `live_session` is reserved for M4. The
//! `EscurelAdmin` service is similarly stubbed in M3.5d.
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

use std::pin::Pin;
use std::sync::Arc;

use escurel_auth::{AuthContext, OidcVerifier, Role};
use escurel_index::{Direction, Indexer, OrderDir};
use escurel_md::PageType;
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
    WikilinkParsed,
};
use escurel_quota::{Dimension, QuotaError};
use futures::Stream;
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
        _req: Request<ValidateRequest>,
    ) -> Result<Response<ValidateResponse>, Status> {
        // Validate lands once the structured frontmatter / wikilink
        // validator (today implicit inside update_page) is split out
        // into a standalone path. The HTTP MCP dispatcher likewise
        // doesn't expose `validate` yet.
        Err(Status::unimplemented(
            "validate lands once the standalone validator is built",
        ))
    }

    type LiveSessionStream = Pin<Box<dyn Stream<Item = Result<LiveAck, Status>> + Send>>;
    async fn live_session(
        &self,
        _req: Request<Streaming<LiveOp>>,
    ) -> Result<Response<Self::LiveSessionStream>, Status> {
        Err(Status::unimplemented("M4 — live CRDT mode"))
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
        Err(Status::unimplemented("M4 — tenant CRUD"))
    }

    async fn tenant_list(
        &self,
        req: Request<TenantListRequest>,
    ) -> Result<Response<TenantListResponse>, Status> {
        self.enforce_admin(req.metadata()).await?;
        Err(Status::unimplemented("M4 — tenant CRUD"))
    }

    async fn tenant_get(
        &self,
        req: Request<TenantGetRequest>,
    ) -> Result<Response<TenantGetResponse>, Status> {
        self.enforce_admin(req.metadata()).await?;
        Err(Status::unimplemented("M4 — tenant CRUD"))
    }

    async fn tenant_update(
        &self,
        req: Request<TenantUpdateRequest>,
    ) -> Result<Response<TenantUpdateResponse>, Status> {
        self.enforce_admin(req.metadata()).await?;
        Err(Status::unimplemented("M4 — tenant CRUD"))
    }

    async fn tenant_delete(
        &self,
        req: Request<TenantDeleteRequest>,
    ) -> Result<Response<TenantDeleteResponse>, Status> {
        self.enforce_admin(req.metadata()).await?;
        Err(Status::unimplemented("M4 — tenant CRUD"))
    }

    type TenantExportStream = StreamOf<TenantExportChunk>;
    async fn tenant_export(
        &self,
        req: Request<TenantExportRequest>,
    ) -> Result<Response<Self::TenantExportStream>, Status> {
        self.enforce_admin(req.metadata()).await?;
        Err(Status::unimplemented("M4 — tenant export"))
    }

    async fn tenant_import(
        &self,
        req: Request<Streaming<TenantImportChunk>>,
    ) -> Result<Response<TenantImportResponse>, Status> {
        self.enforce_admin(req.metadata()).await?;
        Err(Status::unimplemented("M4 — tenant import"))
    }

    async fn audit(&self, req: Request<AuditRequest>) -> Result<Response<AuditResponse>, Status> {
        self.enforce_admin(req.metadata()).await?;
        Err(Status::unimplemented("M4 — audit streaming"))
    }

    type RebuildStream = StreamOf<RebuildProgress>;
    async fn rebuild(
        &self,
        req: Request<RebuildRequest>,
    ) -> Result<Response<Self::RebuildStream>, Status> {
        self.enforce_admin(req.metadata()).await?;
        Err(Status::unimplemented("M4 — rebuild streaming"))
    }

    async fn attach_external(
        &self,
        req: Request<AttachExternalRequest>,
    ) -> Result<Response<AttachExternalResponse>, Status> {
        self.enforce_admin(req.metadata()).await?;
        Err(Status::unimplemented("M4 — external lane attach"))
    }

    async fn embedding_reload(
        &self,
        req: Request<EmbeddingReloadRequest>,
    ) -> Result<Response<EmbeddingReloadResponse>, Status> {
        self.enforce_admin(req.metadata()).await?;
        Err(Status::unimplemented("M5 — embedder hot reload"))
    }

    type CompactLanesStream = StreamOf<CompactProgress>;
    async fn compact_lanes(
        &self,
        req: Request<CompactLanesRequest>,
    ) -> Result<Response<Self::CompactLanesStream>, Status> {
        self.enforce_admin(req.metadata()).await?;
        Err(Status::unimplemented("M4 — lane compaction"))
    }

    async fn quota_get(
        &self,
        req: Request<QuotaGetRequest>,
    ) -> Result<Response<QuotaGetResponse>, Status> {
        self.enforce_admin(req.metadata()).await?;
        // Real impl lands once `QuotaManager` exposes a snapshot
        // method (`remaining(tenant) -> {queries, writes, embeds,
        // sessions}`). Today the manager only exposes
        // `try_consume`, so we'd be guessing.
        Err(Status::unimplemented(
            "M4 — quota snapshot once QuotaManager exposes it",
        ))
    }
}
