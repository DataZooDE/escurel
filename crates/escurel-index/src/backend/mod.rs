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
mod markdown;
mod sql_view;

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use escurel_md::PageType;

use crate::IndexerError;
use crate::acl::AclCaller;
use crate::read::{Direction, Edge, ExpandedPage, InstanceInfo, OrderDir, ResolvedWikilink};
use crate::search::{Granularity, SearchHit};
use crate::validate::Issue;

pub use binding::{BackendBinding, SqlConnector, SqlViewBinding};
pub use markdown::MarkdownBackend;
pub use sql_view::{Materialized, SqlViewBackend, SqlViewError};

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
}

impl BackendKind {
    /// The wire / frontmatter string for this kind.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            BackendKind::Markdown => "markdown",
            BackendKind::SqlView => "sql_view",
        }
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
}

impl SearchMode {
    /// The wire string for this search mode.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            SearchMode::Hybrid => "hybrid",
            SearchMode::LateMaterialized => "late_materialized",
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
