//! Instance backends: *where an instance's data comes from*.
//!
//! escurel's triad (Skills, Instances, Events) is realised as markdown pages in a single referent
//! space `[[skill::id]]`. A skill can drive instances living in other backends (read-only SQL
//! views and rows, ingested documents, remote REST/MCP bindings) while every instance keeps a
//! markdown overlay page for identity, links, ACL and CRDT.
//!
//! There is deliberately NO dispatcher trait here: the planned `InstanceBackend` /
//! `BackendRegistry` seam had exactly one implementation and no caller, and was deleted. The real
//! per-skill dispatch is by probe in the server's read tools (`Indexer::rows_source`, the
//! remote-rows source and the `BackendView` classifier in `escurel-server/src/mcp/backend_view.rs`).
//! The backends themselves (`sql_view`, `document`, `rows`, the `remote` openapi/mcp bindings) are
//! plain modules reached directly. If a second markdown-like backend ever needs a common
//! interface, design it then, around rows, projections and write-back.

mod binding;
#[cfg(feature = "contextualize-llm")]
pub mod contextualize_llm;
pub mod document;
pub mod remote;
pub mod rows;
pub mod rows_write;
mod sql_view;

use escurel_md::{PageKind, parse};

use crate::search::{Granularity, SearchHit};
use crate::{Indexer, IndexerError};

pub use binding::{
    BackendBinding, DocumentBinding, MimeClaim, RemoteBinding, RemoteCursor, RemoteKind,
    RemoteList, RemoteOp, RowsConfig, SqlConnector, SqlViewBinding, mime_claim,
};
#[cfg(feature = "kreuzberg")]
pub use document::KreuzbergExtractor;
pub use document::reclaim_orphan_blobs;
pub use document::{
    Chunk, ChunkConfig, ContextualizeMode, DeterministicProcessor, DocMetadata,
    DocumentIngestWorker, DocumentProcessor, ExtractConfig, ExtractError, ExtractionResult,
    Extractor, IngestOutcome, MediaMetadata, NullExtractor, OcrPolicy, PlainTextExtractor,
    RetainedMediaExtractor, chunk_text, contextualized_chunks, heading_path_at,
    structural_context_prefix,
};
pub use remote::{
    RemoteError, encode_segment, fill_path_template, fill_template, has_dot_segment, json_path_get,
    resolve_projection,
};
pub use rows::{RowRecord, RowsPage, RowsSource};
pub use rows_write::RowWriteError;
pub use sql_view::{
    BindingStatus, MAX_PROJECTION_ROWS, Materialized, SqlViewBackend, SqlViewError,
};
// Crate-internal: `query_instance` allow-lists the `{{target}}` view
// identifier through the same `vw_`-prefix guard the projection path uses.
pub(crate) use sql_view::is_managed_view;
// Crate-internal: the DuckLake attach/secret builders (`snapshot::lake`)
// validate their spliced DSN / data path / credentials with the same
// splice guard the SQL-view backend uses. Do NOT weaken it.
pub(crate) use sql_view::is_safe_sql_fragment;

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
    ///
    /// Deterministic and two-tier: a skill claiming the MIME **exactly**
    /// always beats one claiming it through a type wildcard (`audio/*`,
    /// GH #356); within a tier the first skill by id order wins. That
    /// ordering is what lets an operator add a broad catch-all collection
    /// without diverting uploads away from the narrow skills that named
    /// their MIME.
    ///
    /// `None` ⇒ no handler (the caller parks with `no_handler_skill` and
    /// retains the inbox blob).
    pub async fn document_skill_for_mime(
        &self,
        mime: &str,
    ) -> Result<Option<String>, IndexerError> {
        let mut matches: Vec<(binding::MimeClaim, String)> = Vec::new();
        for skill in self.list_skills().await? {
            if skill.backend.kind == BackendKind::Document
                && let Some(doc) = &skill.backend.document
                && let Some(claim) = binding::mime_claim(&doc.accepts, mime)
            {
                matches.push((claim, skill.id));
            }
        }
        matches.sort();
        Ok(matches.into_iter().next().map(|(_, id)| id))
    }

    /// The backend a skill declares, parsed from its `backend:` block
    /// (markdown default when the skill page is absent or unannotated).
    pub async fn skill_backend(&self, skill_id: &str) -> Result<BackendBinding, IndexerError> {
        let conn = self.conn.lock().await;
        let row: Option<String> = conn
            .query_row(
                "SELECT frontmatter::VARCHAR FROM pages \
                 WHERE page_type = 'skill' AND (slug = ? OR page_id = ?) \
                 ORDER BY (page_id LIKE 'markdown/base/%') LIMIT 1",
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
        if parsed.frontmatter.page_kind != PageKind::Instance {
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
        // A `rows` skill's pages are the rows' linked markdown: writable, under the finer
        // field-level guard `rows_write_rejection`.
        if binding.rows.is_some() || Capabilities::for_kind(binding.kind).writable {
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

    /// The stored page's `layer` frontmatter, or `None` when the page does
    /// not exist or declares no layer (⇒ `overlay`, the default — every
    /// pre-layer page).
    ///
    /// Only the explicit no-row case maps to `None`; any other lookup
    /// failure propagates so the layer guard **fails closed** (codex
    /// review: `.ok()` here would let a DB error unlock base pages).
    pub async fn page_layer(&self, page_id: &str) -> Result<Option<String>, IndexerError> {
        let conn = self.conn.lock().await;
        let layer: Option<String> = match conn.query_row(
            "SELECT json_extract_string(frontmatter, '$.layer') \
             FROM pages WHERE page_id = ?",
            duckdb::params![page_id],
            |r| r.get(0),
        ) {
            Ok(v) => v,
            Err(duckdb::Error::QueryReturnedNoRows) => None,
            Err(e) => return Err(e.into()),
        };
        Ok(layer)
    }

    /// Base-layer write guard (REQ-LAYER-02). Returns `Some(reason)` when an
    /// `update_page` of `content` at `page_id` must be rejected with a
    /// `layer_read_only` `Issue`; `None` when the write is allowed.
    ///
    /// Rejected, fail-closed:
    /// * the page id sits under the reserved
    ///   [`crate::pack::RESERVED_BASE_PREFIX`] — that namespace belongs to
    ///   pack import alone, even for page ids no import has landed yet.
    ///   The check is static (no DB read), so a racing import can neither
    ///   be squatted nor bypassed between guard and write.
    /// * the STORED page carries `layer: base@…` — it was imported from a
    ///   subscribed pack and is read-only at this node. Keying off the
    ///   stored layer (not the draft's) means stripping the `layer:` field
    ///   from the draft is not an unlock.
    /// * the DRAFT declares `layer: base@…` — base pages are stamped by
    ///   the pack-import path only; letting `update_page` fabricate one
    ///   would allow squatting a page id a future import lands on, or
    ///   laundering agent-authored content as pack-authored.
    ///
    /// The seam mirrors [`Self::backend_read_only_rejection`] (the check is
    /// lifted from per-backend-kind to per-page-layer); internal writers —
    /// `seed_from_dir`, the pack import, the ingest/materialise pipelines —
    /// call `Indexer::update_page` directly and are unaffected.
    pub async fn layer_read_only_rejection(
        &self,
        page_id: &str,
        content: &str,
    ) -> Result<Option<String>, IndexerError> {
        if page_id.starts_with(crate::pack::RESERVED_BASE_PREFIX) {
            return Ok(Some(format!(
                "page `{page_id}` is under the reserved `{}` namespace — pack-managed, \
                 read-only at this node; author an overlay page to specialise it",
                crate::pack::RESERVED_BASE_PREFIX
            )));
        }
        if let Some(layer) = self
            .page_layer(page_id)
            .await?
            .filter(|l| l.starts_with("base@"))
        {
            return Ok(Some(format!(
                "page `{page_id}` is layer `{layer}` — imported from a subscribed \
                 pack and read-only at this node; author an overlay page to \
                 specialise it"
            )));
        }
        // A malformed draft falls through to the normal validate path.
        let Ok(parsed) = parse(content) else {
            return Ok(None);
        };
        let draft_layer = parsed
            .frontmatter
            .fields
            .get("layer")
            .and_then(escurel_md::YamlValue::as_str)
            .unwrap_or_default();
        if draft_layer.starts_with("base@") {
            return Ok(Some(format!(
                "draft declares `layer: {draft_layer}` but base-layer pages are \
                 created by pack import only; drop the `layer` field (overlay is \
                 the default)"
            )));
        }
        Ok(None)
    }
}
