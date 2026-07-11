//! The `InstanceBackend` seam: a per-skill strategy for *where an
//! instance's data comes from*.
//!
//! escurel's triad (Skills, Instances, Events) is realised as markdown
//! pages in a single referent space `[[skill::id]]`. This module introduces
//! the abstraction that lets a skill drive instances living in **new
//! backends** — read-only SQL views, ingested documents — while every
//! instance keeps a markdown overlay page for identity, links, ACL, and
//! CRDT (HLD §3, change-request §5.1).
//!
//! ## PR-1 scope (this commit)
//!
//! Only [`MarkdownBackend`] exists; it wraps the existing [`Indexer`] and
//! delegates every call verbatim, so behaviour is bit-identical. The
//! [`BackendRegistry`] maps `skill_id → Arc<dyn InstanceBackend>` and
//! returns the markdown default for any unannotated skill. The trait is
//! shaped to keep today's `as_of` / `scenario` / `granularity` / `filter`
//! knobs — collapsing them into clean DTOs would be a behaviour change,
//! deferred to a later simplification.
//!
//! ## Seams reserved for later PRs
//!
//! - `create_instance` — SQL view materialisation / document ingestion.
//!   Writes still flow through `Indexer::update_page` in PR-1.
//! - `acl_predicate` — the dispatcher computes ACL today
//!   (`Indexer::may_read_instance`); PR-4's row-grain SQL pushes a SQL
//!   predicate here.
//! - `search_contribution` is named for the multi-lane future where the
//!   dispatcher fuses candidates from several backends and must apply ACL
//!   **before** fusion (INV-ACL-FUSION, change-request §5.5). In PR-1 there
//!   is exactly one backend, fusion already happened inside it, and ACL
//!   runs after — equivalent for a single lane. Relocating fusion to the
//!   dispatcher lands with the second searchable backend (PR-2d).

mod binding;
pub mod document;
mod markdown;
pub mod remote;
mod sql_view;

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use escurel_md::{PageType, parse};

use crate::acl::AclCaller;
use crate::read::{Direction, Edge, ExpandedPage, InstanceInfo, OrderDir, ResolvedWikilink};
use crate::search::{Granularity, SearchHit};
use crate::validate::Issue;
use crate::{Indexer, IndexerError};

pub use binding::{
    BackendBinding, DocumentBinding, RemoteBinding, RemoteKind, RemoteOp, SqlConnector,
    SqlViewBinding,
};
#[cfg(feature = "kreuzberg")]
pub use document::KreuzbergExtractor;
pub use document::{
    Chunk, ChunkConfig, ContextualizeMode, DeterministicProcessor, DocMetadata,
    DocumentIngestWorker, DocumentProcessor, ExtractConfig, ExtractError, ExtractionResult,
    Extractor, IngestOutcome, NullExtractor, OcrPolicy, PlainTextExtractor, chunk_text,
    contextualized_chunks, heading_path_at, structural_context_prefix,
};
pub use markdown::MarkdownBackend;
pub use remote::{RemoteError, fill_template, json_path_get, resolve_projection};
pub use sql_view::{
    BindingStatus, MAX_PROJECTION_ROWS, Materialized, SqlViewBackend, SqlViewError,
};
// Crate-internal: `query_instance` allow-lists the `{{target}}` view
// identifier through the same `vw_`-prefix guard the projection path uses.
pub(crate) use sql_view::is_managed_view;

/// Which storage / representation strategy backs a skill's instances.
///
/// `#[non_exhaustive]` so adding `Document` later is not a breaking change
/// for downstream `match`es.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BackendKind {
    /// Native markdown page (today's default for every skill).
    #[default]
    Markdown,
    /// Read-only DuckDB view over an external source (REQ-SQL-*).
    SqlView,
    /// Ingested document → chunks (REQ-DOC-*).
    Document,
    /// Live projection of a remote REST/HTTP endpoint described by an
    /// OpenAPI document (REQ-REMOTE-*). The overlay page's body is fetched
    /// **live on `expand`** (nothing materialised in DuckDB); write-back is
    /// the explicit `write_instance` tool, not `update_page`.
    OpenApi,
    /// Live projection of an upstream MCP server — escurel is the MCP
    /// *client*, calling a tool or reading a resource (REQ-REMOTE-*). Same
    /// live-fetch / explicit-write model as [`BackendKind::OpenApi`].
    Mcp,
    /// A dynamic-workflow **plan** skill: a markdown-file-backed page whose
    /// body is per-phase instructions and whose `phases:`/`verify:`
    /// frontmatter is a deterministic orchestration spec read by the
    /// `escurel-runner-workflow` reducer. Reads and writes behave exactly
    /// like [`BackendKind::Markdown`] (you steer the workflow by editing the
    /// plan page); the distinct kind only lets the runner recognise a plan
    /// skill and `list_skills` report it.
    Workflow,
}

impl BackendKind {
    /// The wire / frontmatter string for this kind.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            BackendKind::Markdown => "markdown",
            BackendKind::SqlView => "sql_view",
            BackendKind::Document => "document",
            BackendKind::OpenApi => "openapi",
            BackendKind::Mcp => "mcp",
            BackendKind::Workflow => "workflow",
        }
    }

    /// Whether this kind is a **live remote (proxy) backend** — its data is
    /// fetched from an external service on every read (no DuckDB copy) and
    /// its overlay body is not CRDT-co-authored. Both `openapi` and `mcp`
    /// share the remote-execution seam (endpoint registry + `RemoteClient`).
    #[must_use]
    pub fn is_remote(self) -> bool {
        matches!(self, BackendKind::OpenApi | BackendKind::Mcp)
    }
}

/// How a backend's instances enter hybrid retrieval.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchMode {
    /// Contributes block/page candidates into the shared hybrid index
    /// (markdown today; document chunks later — both are `blocks` rows).
    Hybrid,
    /// Contributes hits late-materialised from a view's `search_text`
    /// columns at query time (SQL-view backend).
    LateMaterialized,
    /// Contributes **no** dedicated search lane — the backend's remote data
    /// is fetched live and never indexed, so it cannot feed FTS/vector
    /// retrieval. The instance's markdown overlay page is still indexed and
    /// searchable like any page; only the live remote body is not (remote
    /// backends: `openapi` / `mcp`).
    None,
}

impl SearchMode {
    /// The wire string for this search mode.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            SearchMode::Hybrid => "hybrid",
            SearchMode::LateMaterialized => "late_materialized",
            SearchMode::None => "none",
        }
    }
}

/// What a backend can do — reported through `list_skills` so agents and the
/// dispatcher branch without downcasting (REQ-BK-02). `#[non_exhaustive]`
/// so future capability flags are additive.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Capabilities {
    /// Instances can be created / overwritten via `update_page`.
    pub writable: bool,
    /// Finest addressable unit this backend exposes.
    pub granularity: Granularity,
    /// How this backend contributes to search.
    pub search: SearchMode,
    /// Whether CRDT `open_session` / `apply_op` applies to its pages.
    pub supports_crdt: bool,
}

impl Capabilities {
    /// The default capability descriptor for a backend kind. Single
    /// source of truth shared by the backend impls and the `list_skills`
    /// surface, so the reported capabilities never drift from what the
    /// backend actually does. A backend impl MAY still override its own
    /// `capabilities()` (e.g. a per-instance retrieval mode).
    #[must_use]
    pub fn for_kind(kind: BackendKind) -> Self {
        match kind {
            BackendKind::Markdown => Self {
                writable: true,
                granularity: Granularity::Block,
                search: SearchMode::Hybrid,
                supports_crdt: true,
            },
            // SQL views are read-only, view-grain, late-materialised into
            // search, and not CRDT-co-authored (the overlay markdown is).
            BackendKind::SqlView => Self {
                writable: false,
                granularity: Granularity::Page,
                search: SearchMode::LateMaterialized,
                supports_crdt: false,
            },
            // Documents are created via ingestion (not update_page); their
            // chunks are in-band `blocks` rows (hybrid search), and the
            // overlay markdown is co-authorable.
            BackendKind::Document => Self {
                writable: false,
                granularity: Granularity::Block,
                search: SearchMode::Hybrid,
                supports_crdt: true,
            },
            // Remote (proxy) backends: the overlay body is a live remote
            // projection (page-grain), not CRDT-co-authored and not indexed
            // for search. `update_page` is rejected (`writable: false`);
            // write-back to the remote is the explicit `write_instance` tool
            // (see `RemoteBinding::write`).
            BackendKind::OpenApi | BackendKind::Mcp => Self {
                writable: false,
                granularity: Granularity::Page,
                search: SearchMode::None,
                supports_crdt: false,
            },
            // A workflow plan page is a normal markdown page: it is edited to
            // steer the workflow, co-authored via CRDT, and indexed for search
            // like any skill page. Identical to markdown.
            BackendKind::Workflow => Self {
                writable: true,
                granularity: Granularity::Block,
                search: SearchMode::Hybrid,
                supports_crdt: true,
            },
        }
    }
}

/// Read-context threaded through every backend call: the verified caller
/// (for ACL, once backends own it) plus the time-travel cut and scenario
/// overlay every `Indexer` read already takes. Borrowed and `Copy`; cheap.
#[derive(Clone, Copy)]
pub struct BackendCtx<'a> {
    pub caller: AclCaller<'a>,
    pub as_of: Option<&'a str>,
    pub scenario: Option<&'a str>,
}

/// The per-skill strategy for materialising and reading instances.
///
/// PR-1's only impl is [`MarkdownBackend`], which delegates to [`Indexer`].
/// Every method mirrors an existing `Indexer` method so the markdown impl
/// is a verbatim delegate (no logic moves in the refactor).
#[async_trait]
pub trait InstanceBackend: Send + Sync {
    /// The backend discriminant.
    fn kind(&self) -> BackendKind;

    /// The contract for what the dispatcher may call.
    fn capabilities(&self) -> Capabilities;

    /// List a skill's instances (mirrors [`Indexer::list_instances`]).
    async fn list(
        &self,
        ctx: BackendCtx<'_>,
        skill: &str,
        order_by_at: Option<OrderDir>,
        limit: Option<usize>,
        filter: Option<(&str, &str)>,
    ) -> Result<Vec<InstanceInfo>, IndexerError>;

    /// Resolve a `[[skill::id]]` wikilink (mirrors [`Indexer::resolve`]).
    async fn resolve(
        &self,
        ctx: BackendCtx<'_>,
        wikilink: &str,
    ) -> Result<ResolvedWikilink, IndexerError>;

    /// Expand a page's body + frontmatter + outbound links (mirrors
    /// [`Indexer::expand`]).
    async fn expand(
        &self,
        ctx: BackendCtx<'_>,
        page_id: &str,
    ) -> Result<Option<ExpandedPage>, IndexerError>;

    /// Links touching a page (mirrors [`Indexer::neighbours`]).
    async fn neighbours(
        &self,
        ctx: BackendCtx<'_>,
        page_id: &str,
        direction: Direction,
        link_skill_filter: Option<&str>,
    ) -> Result<Vec<Edge>, IndexerError>;

    /// Candidate hits this backend contributes for `q`. For markdown this
    /// is the existing fully-fused hybrid result (see module docs on
    /// INV-ACL-FUSION); future lanes return pre-fusion candidates and the
    /// dispatcher fuses. Mirrors [`Indexer::search_with`].
    #[allow(clippy::too_many_arguments)]
    async fn search_contribution(
        &self,
        ctx: BackendCtx<'_>,
        q: &str,
        k: usize,
        page_type: Option<PageType>,
        skill: Option<&str>,
        granularity: Granularity,
        filter: Option<&serde_json::Value>,
    ) -> Result<Vec<SearchHit>, IndexerError>;

    /// Dry-run authoring validation (mirrors [`Indexer::validate`]).
    async fn validate(
        &self,
        ctx: BackendCtx<'_>,
        page_id: Option<&str>,
        content: &str,
    ) -> Result<Vec<Issue>, IndexerError>;
}

/// Per-tenant map `skill_id → Arc<dyn InstanceBackend>`, with a markdown
/// default for any skill that is absent or declares no `backend:` block.
///
/// Lives on the server's `AppState` (not on `Indexer`), because
/// `MarkdownBackend` holds an `Arc<Indexer>` and nesting the registry on the
/// indexer would create an `Arc` cycle.
pub struct BackendRegistry {
    markdown: Arc<dyn InstanceBackend>,
    by_skill: HashMap<String, Arc<dyn InstanceBackend>>,
}

impl BackendRegistry {
    /// Build a registry whose default (and, in PR-1, only) backend is the
    /// given markdown impl.
    #[must_use]
    pub fn new(markdown: Arc<dyn InstanceBackend>) -> Self {
        Self {
            markdown,
            by_skill: HashMap::new(),
        }
    }

    /// Bind a skill id to a specific backend.
    pub fn bind(&mut self, skill_id: impl Into<String>, backend: Arc<dyn InstanceBackend>) {
        self.by_skill.insert(skill_id.into(), backend);
    }

    /// The backend for `skill_id`; an unbound skill resolves to the
    /// markdown default (REQ-BK-01).
    #[must_use]
    pub fn for_skill(&self, skill_id: &str) -> &Arc<dyn InstanceBackend> {
        self.by_skill.get(skill_id).unwrap_or(&self.markdown)
    }

    /// The markdown default backend (used as the fallback lane).
    #[must_use]
    pub fn markdown(&self) -> &Arc<dyn InstanceBackend> {
        &self.markdown
    }
}

/// Read-path + write-guard helpers the dispatcher uses to make external
/// instances behave uniformly (PR-2c). These live on [`Indexer`] so the
/// MCP handlers, which already hold an `&Indexer`, can call them without the
/// registry being threaded through every handler.
impl Indexer {
    /// Bounded projection of a materialised SQL view's rows (REQ-SQL-06).
    /// `expand` renders this beneath the overlay body; never an unbounded
    /// dump.
    pub async fn project_view(
        &self,
        view: &str,
        limit: usize,
    ) -> Result<Vec<serde_json::Map<String, serde_json::Value>>, SqlViewError> {
        let conn = self.conn.lock().await;
        sql_view::project_view_rows(&conn, view, limit)
    }

    /// Late-materialised SQL-view search **candidates** for `q` (PR-2d,
    /// INV-ACL-FUSION). For every `sql_view` instance (its overlay page
    /// carries `backend_ref.view`), match `q` against the view's
    /// `search_text` columns; a view with ≥1 matching row contributes its
    /// overlay page as a page-grain candidate, ranked by match count.
    ///
    /// **Candidates only** — the dispatcher applies the fail-closed ACL
    /// predicate to these (and to the native lane) *before* RRF fusion, so
    /// no SQL hit can leak cross-owner or displace an allowed hit (spike S3).
    pub async fn sql_view_search_candidates(
        &self,
        q: &str,
        skill_filter: Option<&str>,
    ) -> Result<Vec<SearchHit>, IndexerError> {
        sql_view::search_candidates(self, q, skill_filter).await
    }

    /// Reconstruct every SQL view from its overlay's `backend_ref.source`
    /// (rebuild step, REQ-NF-01). Called at the tail of `rebuild`.
    pub(crate) async fn rebuild_sql_views(&self) -> Result<(), IndexerError> {
        sql_view::reconstruct_views(self).await
    }

    /// Re-probe every SQL-view binding and report drift (REQ-NF-06). Also
    /// reconciles views ⟂ `backend_ref`s: a binding whose view cannot be
    /// reconstructed is reported `backend_unavailable` (no orphans hidden).
    pub async fn validate_bindings(&self) -> Result<Vec<sql_view::BindingStatus>, IndexerError> {
        sql_view::validate_all_bindings(self).await
    }

    /// Current schema fingerprint of a materialised view (for the read-path
    /// fail-closed drift check, REQ-NF-06).
    pub async fn current_view_fingerprint(&self, view: &str) -> Result<String, SqlViewError> {
        let conn = self.conn.lock().await;
        sql_view::schema_fingerprint(&conn, view)
    }

    /// The document skill whose `accepts:` list handles `mime` (REQ-DOC-06).
    /// Deterministic: an exact MIME match wins; returns the first such skill
    /// by id order. `None` ⇒ no handler (the caller parks with
    /// `no_handler_skill` and retains the inbox blob).
    pub async fn document_skill_for_mime(
        &self,
        mime: &str,
    ) -> Result<Option<String>, IndexerError> {
        let mut matches: Vec<String> = Vec::new();
        for skill in self.list_skills().await? {
            if skill.backend.kind == BackendKind::Document
                && let Some(doc) = &skill.backend.document
                && doc.accepts.iter().any(|m| m == mime)
            {
                matches.push(skill.id);
            }
        }
        matches.sort();
        Ok(matches.into_iter().next())
    }

    /// The backend a skill declares, parsed from its `backend:` block
    /// (markdown default when the skill page is absent or unannotated).
    pub async fn skill_backend(&self, skill_id: &str) -> Result<BackendBinding, IndexerError> {
        let conn = self.conn.lock().await;
        let row: Option<String> = conn
            .query_row(
                "SELECT frontmatter::VARCHAR FROM pages \
                 WHERE page_type = 'skill' AND (slug = ? OR page_id = ?) LIMIT 1",
                duckdb::params![skill_id, skill_id],
                |r| r.get(0),
            )
            .ok();
        match row {
            Some(fm_json) => {
                let fm: serde_json::Value = serde_json::from_str(&fm_json)?;
                Ok(BackendBinding::parse(&fm))
            }
            None => Ok(BackendBinding::default()),
        }
    }

    /// Read-only-backend write guard (REQ-BK-03). Returns `Some(reason)` when
    /// an `update_page` of `content` at `page_id` must be rejected with a
    /// `backend_read_only` `Issue`; `None` when the write is allowed.
    ///
    /// Rejected: any `update_page` targeting an instance of a non-writable
    /// backend (`sql_view` | `document`). Those instances are fully
    /// backend-managed — created and updated through the materialise / ingest
    /// pipelines, which write the server-managed `backend_ref` + the view /
    /// chunk-blocks. Allowing `update_page` would let a caller (a) fabricate
    /// an external instance, (b) clobber a document's chunk-blocks with the
    /// single-block markdown path, or (c) **repoint `backend_ref.view` at a
    /// server-side table like `external_credentials` and read it out via
    /// `expand`** (the security hole). Overlay body co-authoring that
    /// preserves the binding is a phase-2 refinement (proper field-level
    /// merge); v1 keeps the binding immutable by refusing the path entirely.
    pub async fn backend_read_only_rejection(
        &self,
        _page_id: &str,
        content: &str,
    ) -> Result<Option<String>, IndexerError> {
        // A malformed draft falls through to the normal validate path.
        let Ok(parsed) = parse(content) else {
            return Ok(None);
        };
        if parsed.frontmatter.page_type != PageType::Instance {
            return Ok(None);
        }
        let skill = parsed
            .frontmatter
            .fields
            .get("skill")
            .and_then(escurel_md::YamlValue::as_str)
            .unwrap_or_default()
            .to_owned();
        if skill.is_empty() {
            return Ok(None);
        }
        let binding = self.skill_backend(&skill).await?;
        if Capabilities::for_kind(binding.kind).writable {
            return Ok(None);
        }
        let kind = binding.kind.as_str();
        let how = match binding.kind {
            BackendKind::Document => "the ingest pipeline (deposit + /ingest)",
            BackendKind::OpenApi | BackendKind::Mcp => {
                "write-back via the write_instance tool (the remote source is canonical)"
            }
            _ => "the materialise path",
        };
        Ok(Some(format!(
            "skill `{skill}` is a read-only `{kind}` backend; its instances are managed by \
             {how}, not update_page (the binding is server-managed and immutable)"
        )))
    }
}
