//! Per-tenant indexer: parses markdown, upserts into DuckDB,
//! audits drift against the canonical markdown on the LaneStore,
//! and rebuilds the DuckDB store from canonical markdown.
//!
//! All write paths run inside a single DuckDB transaction so a
//! mid-write SIGKILL leaves the pages / links / blocks tables
//! atomically rolled back, matching the spec README's failure model.

use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use bytes::Bytes;
use duckdb::{Connection, params};
use escurel_embed::{EmbedError, Embedder, NoopReranker, Reranker};

use crate::retrieval::RetrievalConfig;
use escurel_md::wikilink::parse_wikilinks;
use escurel_md::{PageType, parse};
use escurel_storage::{Key, LaneStore};
use sha2::{Digest, Sha256};
use thiserror::Error;
use tokio::sync::Mutex;

// Re-export the chat-history surface so consumers and tests can
// import the input/output types from the same module path as
// `Indexer` itself.
pub use crate::chat::{
    AppendChatMessage, ChatBackend, ChatMessage, ChatPage, ListChatMessages, SearchChatMessages,
};
// Re-export the events-backend seam (DuckLake PR 9) alongside the
// chat one above — same rationale, same shape.
pub use crate::events::EventsBackend;

/// Hard-coded vector dimension for `blocks.dense_vec` (EmbeddingGemma
/// default). The schema declares `FLOAT[768]`; any embedder passed to
/// `Indexer::new` whose `dim()` does not match is rejected.
pub const BLOCKS_DENSE_VEC_DIM: usize = 768;

/// Per-tenant indexer.
///
/// Holds an open DuckDB connection plus a handle on the canonical
/// markdown lane (any [`LaneStore`] impl). The connection is wrapped
/// in a `tokio::sync::Mutex` because DuckDB connections are
/// single-threaded; concurrent async callers serialise through it.
pub struct Indexer {
    store: Arc<dyn LaneStore>,
    pub(crate) embedder: Arc<dyn Embedder>,
    pub(crate) conn: Mutex<Connection>,
    /// Write-serialization lock. Held across the whole
    /// embed → transaction sequence in [`Self::update_page`] so two
    /// concurrent writes to the same page can't commit out of order
    /// (the slow embedder finishing second and clobbering newer
    /// content). It is NOT the connection mutex: holding `conn`
    /// across a slow (network) embed would block every reader, so the
    /// embed runs while only `write_lock` is held and `conn` is taken
    /// only for the transaction. Mirrors the spec's per-tenant write
    /// lock (`docs/spec/platform.md §Concurrency`).
    write_lock: Mutex<()>,
    /// Dirty counter for the DuckLake publish loop: bumped once per
    /// committed index mutation ([`Self::update_page`],
    /// [`Self::write_document_blocks`] — the shared tail of
    /// [`Self::materialize_document`] — [`Self::merge_from_attached`] and
    /// [`Self::rebuild_with_progress`]). `publish_lake` compares
    /// [`Self::mutation_epoch`] against the last-published value and skips
    /// the publish when nothing changed. Monotone; only equality matters.
    mutation_epoch: AtomicU64,
    tenant: String,
    /// Second-stage cross-encoder reranker. [`NoopReranker`] by default
    /// (identity), so the rerank stage is a no-op until a real reranker
    /// is injected via [`Self::with_reranker`]. Only consulted when
    /// [`Self::retrieval`] has rerank enabled.
    pub(crate) reranker: Arc<dyn Reranker>,
    /// Rerank-stage knobs. Disabled by default — see [`RetrievalConfig`].
    pub(crate) retrieval: RetrievalConfig,
    /// Contextual-retrieval mode applied when (re)materialising document
    /// chunks (GH #216, Variant A). Defaults to
    /// [`ContextualizeMode::Structural`]; the server overrides it from
    /// `ESCUREL_INGEST_CONTEXTUALIZE`. Read by `rebuild_documents` so a
    /// from-scratch rebuild reproduces the same stored chunk text.
    pub(crate) contextualize: crate::backend::ContextualizeMode,
    /// Which physical table `append_chat_message` / `list_chat_messages`
    /// / `delete_chat_history` / `search_chat_messages` read and write
    /// (DuckLake PR 8, Phase B). Unset (→ [`ChatBackend::Local`], the
    /// single-file backend's behaviour, byte-identical to before this PR)
    /// until [`Self::attach_chat_pg`] runs. A `OnceLock` (not a plain
    /// field) because — unlike the reranker/retrieval builders, which run
    /// before the indexer is handed out — the writer boot path only has
    /// an `Arc<Indexer>` (from `SingleFileStore::open`) by the time it
    /// knows whether to attach the chat Postgres connection, so the
    /// setter needs `&self`, not `self`.
    chat_backend: std::sync::OnceLock<ChatBackend>,
    /// Which physical table `capture_event` / `assign_event` /
    /// `list_events` / `list_inbox` read and write (DuckLake PR 9, Phase
    /// B). Unset (→ [`EventsBackend::Local`], the single-file backend's
    /// behaviour, byte-identical to before this PR) until
    /// [`Self::attach_events_pg`] runs. Mirrors [`Self::chat_backend`]
    /// exactly, including the `OnceLock` rationale.
    events_backend: std::sync::OnceLock<EventsBackend>,
}

/// One document chunk handed to the index write path (GH #216, Contextual
/// Retrieval Variant A).
///
/// `body` is the VERBATIM chunk text — it is what `blocks.body` stores, what
/// snippets are cut from and what `expand` displays, so provenance
/// (byte-spans back into the source) survives. `context` is the optional
/// structural situating prefix (`[<title> › <heading path> › p.<page>]`);
/// it is stored separately in `blocks.context` and concatenated with the
/// body only where retrieval looks: the dense embedding input
/// ([`Self::embed_text`]), the BM25 FTS index (which indexes both columns)
/// and the rerank passage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexChunk {
    /// Structural situating prefix, `None` for an uncontextualised chunk.
    pub context: Option<String>,
    /// Verbatim chunk text (display + provenance).
    pub body: String,
}

impl IndexChunk {
    /// A chunk with no situating context (legacy representation).
    pub fn plain(body: impl Into<String>) -> Self {
        Self {
            context: None,
            body: body.into(),
        }
    }

    /// A chunk with an optional situating context.
    pub fn contextualized(context: Option<String>, body: impl Into<String>) -> Self {
        Self {
            context,
            body: body.into(),
        }
    }

    /// The text the dense embedder sees: `"<context>\n<body>"` when a
    /// context is present, the bare body otherwise.
    #[must_use]
    pub fn embed_text(&self) -> std::borrow::Cow<'_, str> {
        match &self.context {
            Some(c) => std::borrow::Cow::Owned(format!("{c}\n{}", self.body)),
            None => std::borrow::Cow::Borrowed(self.body.as_str()),
        }
    }
}

/// Per-page progress event emitted by
/// [`Indexer::rebuild_with_progress`]. Borrowed so the callback
/// can receive a `&str` without forcing an allocation per page;
/// the MCP handlers serialize it into the response at the boundary.
#[derive(Debug)]
pub struct RebuildProgress<'a> {
    pub done: u64,
    pub total: u64,
    pub current_page: &'a str,
}

/// Two-way drift between canonical markdown and the DuckDB index.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct AuditDrift {
    /// Markdown files present on the LaneStore but absent from
    /// the `pages` table — typically a new file the indexer
    /// hasn't seen yet.
    pub markdown_not_in_duckdb: Vec<String>,

    /// Page rows in DuckDB whose backing markdown file has been
    /// removed from the LaneStore — typically a delete the
    /// indexer hasn't been told about.
    pub indexed_but_no_markdown: Vec<String>,
}

impl AuditDrift {
    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.markdown_not_in_duckdb.is_empty() && self.indexed_but_no_markdown.is_empty()
    }
}

#[derive(Debug, Error)]
pub enum IndexerError {
    #[error("duckdb error: {0}")]
    Duckdb(#[from] duckdb::Error),
    #[error("lane store error: {0}")]
    Store(#[from] escurel_storage::StoreError),
    #[error("markdown parse error: {0}")]
    Md(#[from] escurel_md::ParseError),
    #[error("invalid key: {0}")]
    Key(#[from] escurel_storage::KeyError),
    #[error("invalid utf-8 in markdown body for {page_id}")]
    NotUtf8 { page_id: String },
    #[error("serde_json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("embedder error: {0}")]
    Embed(#[from] EmbedError),
    #[error(
        "embedder dim {got} does not match schema column dim {expected}; \
         the blocks.dense_vec column is hard-coded to {expected} (EmbeddingGemma default)"
    )]
    EmbedderDimMismatch { expected: usize, got: usize },
    #[error("invalid external source for attach: {reason}")]
    InvalidExternalSource { reason: &'static str },
    #[error("invalid chat list cursor: {0}")]
    InvalidCursor(String),
    #[error("crdt error: {0}")]
    Crdt(#[from] escurel_crdt::Error),
    #[error("seed io error at {path}: {source}")]
    SeedIo {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("meta-skill protected: {reason}")]
    MetaSkillProtected { reason: String },
    #[error("transfer rejected: {0}")]
    Transfer(String),
    #[error("pack skill `{skill}` has no skill page in this tenant")]
    PackSkillMissing { skill: String },
    #[error("promotion_not_eligible: {reason}")]
    PromotionNotEligible { reason: String },
}

/// What a DuckDB→DuckDB merge does on a `page_id` already in the target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OnCollision {
    /// Default: import only new `page_id`s, leave existing rows untouched
    /// (additive, idempotent — a re-run resumes cleanly).
    Skip,
    /// Delete the colliding instances in the target, then insert the source's.
    Replace,
    /// Abort the whole merge if any `page_id` collides (safest for a one-shot
    /// migration into a fresh tenant).
    Error,
}

/// Summary of a [`Indexer::merge_from_attached`] run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MergeReport {
    /// Instance pages in the attached source.
    pub source_pages: usize,
    /// Source `page_id`s that already existed in the target.
    pub collisions: usize,
}

impl Indexer {
    /// Build a per-tenant indexer.
    ///
    /// # Errors
    ///
    /// Returns [`IndexerError::EmbedderDimMismatch`] when the
    /// supplied `embedder.dim()` does not match
    /// [`BLOCKS_DENSE_VEC_DIM`]. Mismatches are detected at
    /// construction time so we never end up writing a wrong-shape
    /// vector into a typed `FLOAT[768]` column.
    pub fn new(
        store: Arc<dyn LaneStore>,
        embedder: Arc<dyn Embedder>,
        conn: Connection,
        tenant: impl Into<String>,
    ) -> Result<Self, IndexerError> {
        if embedder.dim() != BLOCKS_DENSE_VEC_DIM {
            return Err(IndexerError::EmbedderDimMismatch {
                expected: BLOCKS_DENSE_VEC_DIM,
                got: embedder.dim(),
            });
        }
        Ok(Self {
            store,
            embedder,
            conn: Mutex::new(conn),
            write_lock: Mutex::new(()),
            mutation_epoch: AtomicU64::new(0),
            tenant: tenant.into(),
            reranker: Arc::new(NoopReranker),
            retrieval: RetrievalConfig::disabled(),
            contextualize: crate::backend::ContextualizeMode::default(),
            chat_backend: std::sync::OnceLock::new(),
            events_backend: std::sync::OnceLock::new(),
        })
    }

    /// The chat backend this indexer is currently wired to —
    /// [`ChatBackend::Local`] until [`Self::attach_chat_pg`] has run.
    pub(crate) fn chat_backend(&self) -> ChatBackend {
        self.chat_backend
            .get()
            .cloned()
            .unwrap_or(ChatBackend::Local)
    }

    /// `true` once [`Self::attach_chat_pg`] has wired this indexer onto
    /// the shared chat Postgres table. `escurel-server`'s ducklake-reader
    /// dispatch gate (DuckLake PR 8) consults this to decide whether
    /// `append_message`/`list_messages` are servable on a reader — a
    /// reader boots with `crdt_backend: None` and no local write surface,
    /// but chat rows now live in a table every replica can reach, so
    /// those two tools stop being reader-unsupported exactly when this is
    /// `true`.
    #[must_use]
    pub fn has_shared_chat(&self) -> bool {
        matches!(self.chat_backend(), ChatBackend::AttachedPostgres { .. })
    }

    /// Attach the shared chat Postgres table onto THIS indexer's own
    /// connection, read-write and idempotently (DuckLake PR 8, mirrors
    /// [`Self::attach_lake`]'s "attach on my own connection" shape), and
    /// point every subsequent `append_chat_message` / `list_chat_messages`
    /// / `delete_chat_history` / `search_chat_messages` call at it instead
    /// of the local `chat_messages` table. Idempotent to call twice (the
    /// underlying `ATTACH IF NOT EXISTS` / `CREATE TABLE IF NOT EXISTS`
    /// are); the server calls this once at boot for a ducklake writer OR
    /// reader, reusing `LakeConfig::catalog_dsn` — no separate chat config
    /// needed.
    ///
    /// # Errors
    ///
    /// See [`crate::snapshot::attach_chat_pg`].
    pub async fn attach_chat_pg(
        &self,
        catalog_dsn: &str,
    ) -> Result<(), crate::snapshot::SnapshotError> {
        {
            let conn = self.conn.lock().await;
            crate::snapshot::attach_chat_pg(&conn, catalog_dsn)?;
        }
        let _ = self.chat_backend.set(ChatBackend::AttachedPostgres {
            alias: crate::snapshot::CHAT_PG_ALIAS.to_owned(),
        });
        Ok(())
    }

    /// The events backend this indexer is currently wired to —
    /// [`EventsBackend::Local`] until [`Self::attach_events_pg`] has run.
    pub(crate) fn events_backend(&self) -> EventsBackend {
        self.events_backend
            .get()
            .cloned()
            .unwrap_or(EventsBackend::Local)
    }

    /// `true` once [`Self::attach_events_pg`] has wired this indexer onto
    /// the shared events Postgres table. `escurel-server`'s ducklake-
    /// reader dispatch gate (DuckLake PR 9) consults this to decide
    /// whether `capture_event`/`assign_event`/`list_events`/`list_inbox`
    /// are servable on a reader — a reader boots with no local write
    /// surface, but event rows now live in a table every replica can
    /// reach, so those four tools stop being reader-unsupported exactly
    /// when this is `true`.
    #[must_use]
    pub fn has_shared_events(&self) -> bool {
        matches!(
            self.events_backend(),
            EventsBackend::AttachedPostgres { .. }
        )
    }

    /// Attach the shared events Postgres table onto THIS indexer's own
    /// connection, read-write and idempotently (DuckLake PR 9, mirrors
    /// [`Self::attach_chat_pg`] exactly), and point every subsequent
    /// `capture_event` / `assign_event` / `list_events` / `list_inbox`
    /// call at it instead of the local `events` table. Idempotent to
    /// call twice; the server calls this once at boot for a ducklake
    /// writer OR reader, reusing `LakeConfig::catalog_dsn` — no separate
    /// events config needed.
    ///
    /// # Errors
    ///
    /// See [`crate::snapshot::attach_events_pg`].
    pub async fn attach_events_pg(
        &self,
        catalog_dsn: &str,
    ) -> Result<(), crate::snapshot::SnapshotError> {
        {
            let conn = self.conn.lock().await;
            crate::snapshot::attach_events_pg(&conn, catalog_dsn)?;
        }
        let _ = self.events_backend.set(EventsBackend::AttachedPostgres {
            alias: crate::snapshot::EVENTS_PG_ALIAS.to_owned(),
        });
        Ok(())
    }

    /// Attach a second-stage reranker and its [`RetrievalConfig`].
    /// Without this the indexer reranks with [`NoopReranker`] and the
    /// rerank stage is disabled (today's first-stage-only behaviour).
    /// The server calls this when the `[retrieval]` config enables
    /// rerank and a concrete cross-encoder has loaded.
    #[must_use]
    pub fn with_reranker(
        mut self,
        reranker: Arc<dyn Reranker>,
        retrieval: RetrievalConfig,
    ) -> Self {
        self.reranker = reranker;
        self.retrieval = retrieval;
        self
    }

    /// Set the [`RetrievalConfig`] directly, keeping the current reranker
    /// ([`NoopReranker`] unless [`Self::with_reranker`] was used). Lets the
    /// server enable Matryoshka two-pass vector search without a reranker.
    #[must_use]
    pub fn with_retrieval(mut self, retrieval: RetrievalConfig) -> Self {
        self.retrieval = retrieval;
        self
    }

    /// Whether the post-fusion rerank stage runs.
    #[must_use]
    pub fn rerank_enabled(&self) -> bool {
        self.retrieval.rerank_enabled()
    }

    /// First-stage candidate-pool size for caller `k`: the larger
    /// rerank pool when rerank is on, else `k` unchanged. The search
    /// dispatcher fetches this many fused candidates, reranks them, then
    /// truncates back to `k`.
    #[must_use]
    pub fn rerank_candidate_pool(&self, k: usize) -> usize {
        if self.retrieval.rerank_enabled() {
            k.max(self.retrieval.rerank_candidates())
        } else {
            k
        }
    }

    /// Set the document contextual-retrieval mode (GH #216, Variant A).
    /// Builder style; defaults to [`crate::backend::ContextualizeMode::Structural`].
    #[must_use]
    pub fn with_contextualize(mut self, mode: crate::backend::ContextualizeMode) -> Self {
        self.contextualize = mode;
        self
    }

    /// The document contextual-retrieval mode this indexer applies.
    #[must_use]
    pub fn contextualize_mode(&self) -> crate::backend::ContextualizeMode {
        self.contextualize
    }

    /// Tenant id this indexer was bound to at construction.
    #[must_use]
    pub fn tenant(&self) -> &str {
        &self.tenant
    }

    /// Current value of the dirty counter — bumped once per committed
    /// index mutation. The DuckLake publish loop records the epoch it
    /// published and skips the next publish when this still matches
    /// (see the field doc on `mutation_epoch`).
    #[must_use]
    pub fn mutation_epoch(&self) -> u64 {
        self.mutation_epoch.load(Ordering::Acquire)
    }

    /// Bump the dirty counter after a committed mutation. Call at the
    /// tail of every write path that changed `pages`/`links`/`blocks`
    /// (AFTER the transaction committed — a rolled-back write must not
    /// dirty the epoch).
    fn bump_mutation_epoch(&self) {
        self.mutation_epoch.fetch_add(1, Ordering::Release);
    }

    /// Attach a DuckLake onto THIS indexer's own connection, read-write
    /// and idempotently (`ATTACH IF NOT EXISTS`, DuckLake PR 6). Lets a
    /// writer boot with the lake already attached so a later publish
    /// (`crate::snapshot::publish_lake`) — or a future admin publish
    /// tool — never pays a fresh `ATTACH` round-trip. `conn`'s field
    /// visibility is `pub(crate)`, so this method is the only way a
    /// caller outside `escurel-index` can reach the live connection for
    /// an attach.
    ///
    /// # Errors
    ///
    /// See [`crate::snapshot::attach_lake`].
    pub async fn attach_lake(
        &self,
        cfg: &crate::snapshot::LakeConfig,
    ) -> Result<(), crate::snapshot::SnapshotError> {
        let conn = self.conn.lock().await;
        crate::snapshot::attach_lake(&conn, cfg, false)
    }

    /// Take the per-tenant write lock — the same lock
    /// [`Self::update_page`] serialises writers through. `publish_lake`
    /// holds it across the whole publish so a snapshot can't interleave
    /// with an in-flight ingest's embed→write sequence.
    pub(crate) async fn write_guard(&self) -> tokio::sync::MutexGuard<'_, ()> {
        self.write_lock.lock().await
    }

    /// The canonical markdown [`LaneStore`] this indexer reads/writes.
    /// Cloned `Arc` handle for admin lane-introspection tools that need
    /// raw access to stored bytes.
    #[must_use]
    pub fn lane_store(&self) -> Arc<dyn LaneStore> {
        Arc::clone(&self.store)
    }

    /// Upsert the page identified by `page_id` from the markdown
    /// `content` blob, inside a single DuckDB transaction.
    ///
    /// `page_id` is the caller's stable handle for this page —
    /// during bootstrap we use the markdown file's relative path
    /// within the tenant (e.g. `markdown/skills/customer.md`).
    /// ULID + slug semantics arrive in a later PR.
    pub async fn update_page(&self, page_id: &str, content: &str) -> Result<(), IndexerError> {
        // Serialise the whole embed → write sequence through the
        // dedicated write lock (NOT the connection mutex) so two
        // concurrent writers can't commit out of order. Held for the
        // duration of this call; the embed below runs while readers
        // keep free access to `conn`.
        let _write = self.write_lock.lock().await;

        // The mandatory `escurel` meta-skill is protected: a write that
        // drops its skill identity or one of its established sections is
        // rejected (operators may append, never remove). See
        // `docs/contract/agent-interface.md` locked decision 3. The
        // baseline is whatever the meta-skill currently carries — empty
        // on first write, so the initial shape (canonical or a tenant's
        // own) is free.
        if crate::meta_skill::is_meta_skill_page(page_id) {
            let existing_body: Option<String> = {
                let conn = self.conn.lock().await;
                conn.query_row(
                    "SELECT body FROM blocks WHERE page_id = ?",
                    params![page_id],
                    |row| row.get(0),
                )
                .ok()
            };
            let existing_sections = existing_body
                .as_deref()
                .map(crate::meta_skill::section_headers)
                .unwrap_or_default();
            if let Some(reason) =
                crate::meta_skill::meta_skill_violation(content, &existing_sections)
            {
                return Err(IndexerError::MetaSkillProtected { reason });
            }
        }
        let parsed = parse(content)?;

        // Persist the canonical markdown to the LaneStore BEFORE indexing.
        // `update_page` previously wrote only the DuckDB index, leaving the
        // LaneStore (the source of truth `rebuild`/`audit` read) out of sync —
        // so a lost or dropped DuckDB rebuilt to an EMPTY corpus, breaking the
        // crash-recovery contract (docs/spec/storage.md) and leaving `audit`
        // permanently dirty. Writing the lane first (canonical) then the index
        // (derived) keeps them in sync and makes the index safely rebuildable.
        // `page_id` is the lane key (`markdown/<relpath>`), as `seed_from_dir`
        // and `ensure_meta_skill` already rely on.
        {
            let key = Key::new(self.tenant.as_str(), page_id.to_owned())?;
            self.store
                .write(&key, Bytes::from(content.to_owned()))
                .await?;
        }

        let frontmatter_json = mapping_to_json(&parsed.frontmatter.fields)?;
        let body_hash = hash_body(content);
        let page_type_str = match parsed.frontmatter.page_type {
            PageType::Skill => "skill",
            PageType::Instance => "instance",
        };
        let skill = parsed
            .frontmatter
            .fields
            .get("skill")
            .and_then(escurel_md::YamlValue::as_str)
            .or_else(|| {
                // Skill pages declare themselves via `id:`, not `skill:`.
                parsed
                    .frontmatter
                    .fields
                    .get("id")
                    .and_then(escurel_md::YamlValue::as_str)
            })
            .unwrap_or("")
            .to_owned();
        let at_ts = parsed
            .frontmatter
            .fields
            .get("at")
            .and_then(escurel_md::YamlValue::as_str)
            .map(str::to_owned);
        // Mirror frontmatter `scenario:` into the column. NULL (absent)
        // = the shared base timeline; a value marks a what-if overlay.
        let scenario = parsed
            .frontmatter
            .fields
            .get("scenario")
            .and_then(escurel_md::YamlValue::as_str)
            .map(str::to_owned);
        // `slug` is the wikilink-target id (e.g. `acme-corp`). Wikilinks
        // `[[customer::acme-corp]]` resolve via `WHERE skill = ? AND
        // slug = ?`. Skill pages declare it via the same `id:` field.
        let slug = parsed
            .frontmatter
            .fields
            .get("id")
            .and_then(escurel_md::YamlValue::as_str)
            .map(str::to_owned);
        let body_text = parsed.body.to_owned();
        let wikilinks = parse_wikilinks(&body_text);
        // Typed links also live in frontmatter (e.g. `about:`,
        // `derived_from:`, `primary_sponsor:`). Index those too so the
        // graph reflects relationships an instance declares in its
        // frontmatter, not only in its body.
        let fm_wikilinks = frontmatter_wikilinks(&parsed.frontmatter.fields);

        // Embed WITHOUT holding the connection mutex. Out-of-order
        // commits — the original hazard a codex review of M2.1 caught,
        // where a slow embed finishes second and overwrites newer
        // content — are prevented by `write_lock` (taken at the top of
        // this fn), which serialises writers through the whole
        // embed → write sequence. Keeping `conn` free during the
        // (potentially network-bound) embed means concurrent reads
        // (search / list) are no longer blocked. This matches the
        // spec's per-tenant write-RwLock model
        // (`docs/spec/platform.md §Concurrency`). See
        // `docs/notes/discovered/2026-05-24-update-page-embed-order.md`.
        let embeddings = self.embedder.embed(&[body_text.as_str()]).await?;
        let dense_vec = embeddings.into_iter().next().ok_or_else(|| {
            IndexerError::Embed(EmbedError::Backend(
                "embedder returned no vectors for a single-text batch".to_owned(),
            ))
        })?;
        if dense_vec.len() != BLOCKS_DENSE_VEC_DIM {
            return Err(IndexerError::EmbedderDimMismatch {
                expected: BLOCKS_DENSE_VEC_DIM,
                got: dense_vec.len(),
            });
        }
        let dense_vec_sql = format_vector_literal(&dense_vec);

        // Take the connection mutex only for the transaction.
        let mut conn = self.conn.lock().await;
        let tx = conn.transaction()?;

        // pages: upsert via DELETE + INSERT to keep semantics
        // straightforward without depending on an ON CONFLICT clause
        // that varies by DuckDB version.
        tx.execute("DELETE FROM pages WHERE page_id = ?", params![page_id])?;
        tx.execute(
            "INSERT INTO pages \
             (page_id, slug, skill, page_type, frontmatter, body_hash, at_ts, scenario, created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?::JSON, ?, \
                     TRY_CAST(? AS TIMESTAMP), ?, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP)",
            params![
                page_id,
                slug,
                skill,
                page_type_str,
                frontmatter_json,
                body_hash,
                at_ts,
                scenario,
            ],
        )?;

        // links: full refresh for this src page.
        tx.execute("DELETE FROM links WHERE src_page = ?", params![page_id])?;
        for wl in &wikilinks {
            let link_skill = wl.skill.as_deref().unwrap_or("");
            let dst_page = wl.id.as_deref().unwrap_or("");
            if dst_page.is_empty() {
                continue;
            }
            tx.execute(
                "INSERT OR IGNORE INTO links \
                 (src_page, src_anchor, src_field, dst_page, dst_anchor, link_skill, link_version) \
                 VALUES (?, '', NULL, ?, ?, ?, ?)",
                params![
                    page_id,
                    dst_page,
                    wl.anchor.as_deref().unwrap_or(""),
                    link_skill,
                    wl.version.as_deref(),
                ],
            )?;
        }
        // Frontmatter-sourced links carry the originating field in
        // `src_field` (e.g. `frontmatter.about`). `INSERT OR IGNORE`
        // lets a body link for the same edge win the (PK) row; the edge
        // is reachable from `neighbours` either way.
        for (field, wl) in &fm_wikilinks {
            let link_skill = wl.skill.as_deref().unwrap_or("");
            let dst_page = wl.id.as_deref().unwrap_or("");
            if dst_page.is_empty() {
                continue;
            }
            let src_field = format!("frontmatter.{field}");
            tx.execute(
                "INSERT OR IGNORE INTO links \
                 (src_page, src_anchor, src_field, dst_page, dst_anchor, link_skill, link_version) \
                 VALUES (?, '', ?, ?, ?, ?, ?)",
                params![
                    page_id,
                    src_field,
                    dst_page,
                    wl.anchor.as_deref().unwrap_or(""),
                    link_skill,
                    wl.version.as_deref(),
                ],
            )?;
        }

        // blocks: single block per page for now (whole body).
        // Block-anchor splitting lands in a later M2 PR.
        tx.execute("DELETE FROM blocks WHERE page_id = ?", params![page_id])?;
        let block_id = format!("{page_id}:blk-0");
        let dense_vec_literal = format!("{dense_vec_sql}::FLOAT[{BLOCKS_DENSE_VEC_DIM}]");
        let block_insert_sql = format!(
            "INSERT INTO blocks \
             (block_id, page_id, anchor, ordinal, body, dense_vec, skill, page_type, at_ts, scenario) \
             VALUES (?, ?, 'blk-0', 0, ?, {dense_vec_literal}, ?, ?, TRY_CAST(? AS TIMESTAMP), ?)",
        );
        tx.execute(
            &block_insert_sql,
            params![
                block_id,
                page_id,
                body_text,
                skill,
                page_type_str,
                at_ts,
                scenario
            ],
        )?;

        tx.commit()?;
        self.bump_mutation_epoch();
        Ok(())
    }

    /// Materialise a document instance: write the overlay page (its
    /// frontmatter carries the `backend_ref`) and index `chunks` as N
    /// `blocks` rows under the **one** `page_id` (distinct `chunk-<i>`
    /// anchors), so a document instance is structurally a page-with-blocks
    /// (REQ-DOC-03). Each chunk is embedded; the whole write is one
    /// transaction under the per-tenant write lock (the brief locked phase
    /// of REQ-DOC-07 / REQ-NF-04 — extraction already happened off-lock).
    pub async fn materialize_document(
        &self,
        page_id: &str,
        overlay_content: &str,
        chunks: &[IndexChunk],
    ) -> Result<(), IndexerError> {
        // Embed every chunk in one batch (off the connection mutex), then hand
        // the precomputed vectors to the write path. Splitting embed from write
        // lets the offline batch loader supply vectors from a separate (e.g.
        // Gemini Batch API) pass and lets a DuckDB→DuckDB transfer carry
        // vectors verbatim without ever re-embedding.
        //
        // The embedder sees the CONTEXTUALISED text (`context\n body`, GH
        // #216) while `blocks.body` keeps the verbatim chunk.
        let embed_texts: Vec<std::borrow::Cow<'_, str>> =
            chunks.iter().map(IndexChunk::embed_text).collect();
        let chunk_refs: Vec<&str> = embed_texts
            .iter()
            .map(std::convert::AsRef::as_ref)
            .collect();
        let embeddings = if chunk_refs.is_empty() {
            Vec::new()
        } else {
            self.embedder.embed(&chunk_refs).await?
        };
        self.write_document_blocks(page_id, overlay_content, chunks, &embeddings)
            .await
    }

    /// Write a document instance (overlay page + N chunk `blocks`) from
    /// **precomputed** chunk vectors — the embed-free half of
    /// [`Self::materialize_document`]. Each `vectors[i]` is the embedding of
    /// `chunks[i]` and is stored verbatim in `blocks.dense_vec` (no embedder is
    /// consulted), so callers that embedded elsewhere (offline batch loader)
    /// pay the embedding cost exactly once. `vectors.len()` must equal
    /// `chunks.len()` and each vector must be [`BLOCKS_DENSE_VEC_DIM`] long.
    pub async fn write_document_blocks(
        &self,
        page_id: &str,
        overlay_content: &str,
        chunks: &[IndexChunk],
        vectors: &[Vec<f32>],
    ) -> Result<(), IndexerError> {
        if vectors.len() != chunks.len() {
            return Err(IndexerError::EmbedderDimMismatch {
                expected: chunks.len(),
                got: vectors.len(),
            });
        }
        for v in vectors {
            if v.len() != BLOCKS_DENSE_VEC_DIM {
                return Err(IndexerError::EmbedderDimMismatch {
                    expected: BLOCKS_DENSE_VEC_DIM,
                    got: v.len(),
                });
            }
        }

        let _write = self.write_lock.lock().await;
        let parsed = parse(overlay_content)?;

        // Canonical markdown first (overlay), as in update_page.
        {
            let key = Key::new(self.tenant.as_str(), page_id.to_owned())?;
            self.store
                .write(&key, Bytes::from(overlay_content.to_owned()))
                .await?;
        }

        let frontmatter_json = mapping_to_json(&parsed.frontmatter.fields)?;
        let body_hash = hash_body(overlay_content);
        let skill = parsed
            .frontmatter
            .fields
            .get("skill")
            .and_then(escurel_md::YamlValue::as_str)
            .unwrap_or("")
            .to_owned();
        let slug = parsed
            .frontmatter
            .fields
            .get("id")
            .and_then(escurel_md::YamlValue::as_str)
            .map(str::to_owned);
        let at_ts = parsed
            .frontmatter
            .fields
            .get("at")
            .and_then(escurel_md::YamlValue::as_str)
            .map(str::to_owned);
        let scenario = parsed
            .frontmatter
            .fields
            .get("scenario")
            .and_then(escurel_md::YamlValue::as_str)
            .map(str::to_owned);

        let mut conn = self.conn.lock().await;
        let tx = conn.transaction()?;
        tx.execute("DELETE FROM pages WHERE page_id = ?", params![page_id])?;
        tx.execute("DELETE FROM links WHERE src_page = ?", params![page_id])?;
        tx.execute(
            "INSERT INTO pages \
             (page_id, slug, skill, page_type, frontmatter, body_hash, at_ts, scenario, created_at, updated_at) \
             VALUES (?, ?, ?, 'instance', ?::JSON, ?, TRY_CAST(? AS TIMESTAMP), ?, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP)",
            params![page_id, slug, skill, frontmatter_json, body_hash, at_ts, scenario],
        )?;
        // Chunks become the page's blocks (one row per chunk). `body` is the
        // verbatim chunk text; `context` holds the structural situating
        // prefix (GH #216) — concatenated only at embed/FTS/rerank time.
        tx.execute("DELETE FROM blocks WHERE page_id = ?", params![page_id])?;
        for (i, (chunk, emb)) in chunks.iter().zip(vectors.iter()).enumerate() {
            let anchor = format!("chunk-{i}");
            let block_id = format!("{page_id}:{anchor}");
            let dense_vec_literal = format!(
                "{}::FLOAT[{BLOCKS_DENSE_VEC_DIM}]",
                format_vector_literal(emb)
            );
            let sql = format!(
                "INSERT INTO blocks \
                 (block_id, page_id, anchor, ordinal, body, context, dense_vec, skill, page_type, at_ts, scenario) \
                 VALUES (?, ?, ?, ?, ?, ?, {dense_vec_literal}, ?, 'instance', TRY_CAST(? AS TIMESTAMP), ?)",
            );
            tx.execute(
                &sql,
                params![
                    block_id,
                    page_id,
                    anchor,
                    i as i64,
                    chunk.body,
                    chunk.context,
                    skill,
                    at_ts,
                    scenario
                ],
            )?;
        }
        tx.commit()?;
        self.bump_mutation_epoch();
        Ok(())
    }

    /// Drop + recreate the `blocks` HNSW vector index. Per-row HNSW
    /// maintenance is the slow path on a large bulk load (the offline batch
    /// loader, or a DuckDB→DuckDB merge); the fast pattern is to insert all
    /// rows first and rebuild the index once at the end. Vector search stays
    /// *correct* throughout — `search_with` ranks by `array_cosine_distance`,
    /// the HNSW index only accelerates it — so this is purely a speed knob.
    pub async fn reindex_vectors(&self) -> Result<(), IndexerError> {
        let conn = self.conn.lock().await;
        conn.execute_batch(
            "DROP INDEX IF EXISTS hnsw_blocks_vec; \
             CREATE INDEX hnsw_blocks_vec ON blocks USING HNSW (dense_vec) \
             WITH (metric = 'cosine', ef_construction = 128, ef_search = 64, M = 16);",
        )?;
        Ok(())
    }

    /// Merge `pages`/`links`/`blocks` from an already-[`attach_external`]ed
    /// read-only DuckDB (`alias`) into this tenant — the import half of the
    /// offline batch loader. Rows (including `blocks.dense_vec`) copy verbatim
    /// DuckDB→DuckDB, so **no re-embedding** happens. The caller must have
    /// validated the source's embedder `model_id`/`dim` + schema version
    /// against this tenant (see the loader manifest) BEFORE calling.
    ///
    /// HNSW is dropped before the bulk insert and recreated after (the
    /// bulk-load fast path); the BM25 FTS snapshot is refreshed once. Blobs +
    /// overlay markdown are NOT moved here — copy them into the LaneStore
    /// first (content-addressed, idempotent), then call this so a crash never
    /// leaves rows referencing missing blobs.
    ///
    /// Only the row INSERTs are transactional. The HNSW drop/recreate and the
    /// FTS refresh sit OUTSIDE that transaction, so a crash after the commit but
    /// before they finish can leave committed rows with a missing HNSW index or
    /// a stale FTS snapshot. Both are self-healing: vector search stays correct
    /// without HNSW (a cosine scan; see `search.rs`), and simply re-running the
    /// merge (or `reindex_vectors` + `refresh_fts`, or a `rebuild`) restores the
    /// index + snapshot — the row state is never corrupted, only its indexes.
    /// Count source `page_id`s in an already-attached DuckDB (`alias`) that
    /// already exist in this tenant. Lets a caller preflight a transfer (e.g.
    /// abort an `error`-collision policy BEFORE copying any files) without
    /// running the merge. Cheap, read-only.
    pub async fn attached_page_collisions(&self, alias: &str) -> Result<usize, IndexerError> {
        if !is_valid_attach_alias(alias) {
            return Err(IndexerError::InvalidExternalSource {
                reason: "attach alias is not a valid identifier",
            });
        }
        let conn = self.conn.lock().await;
        let n: i64 = conn.query_row(
            &format!(
                "SELECT count(*) FROM {alias}.pages a \
                 WHERE a.page_id IN (SELECT page_id FROM pages)"
            ),
            [],
            |r| r.get(0),
        )?;
        Ok(n as usize)
    }

    pub async fn merge_from_attached(
        &self,
        alias: &str,
        on_collision: OnCollision,
    ) -> Result<MergeReport, IndexerError> {
        if !is_valid_attach_alias(alias) {
            return Err(IndexerError::InvalidExternalSource {
                reason: "attach alias is not a valid identifier",
            });
        }
        let mut conn = self.conn.lock().await;

        let source_pages: i64 =
            conn.query_row(&format!("SELECT count(*) FROM {alias}.pages"), [], |r| {
                r.get(0)
            })?;
        let collisions: i64 = conn.query_row(
            &format!(
                "SELECT count(*) FROM {alias}.pages a \
                 WHERE a.page_id IN (SELECT page_id FROM pages)"
            ),
            [],
            |r| r.get(0),
        )?;
        if matches!(on_collision, OnCollision::Error) && collisions > 0 {
            return Err(IndexerError::Transfer(format!(
                "{collisions} page_id(s) already exist in the target (on_collision=error)"
            )));
        }

        // Per-row HNSW maintenance is slow; drop the index, bulk-insert, rebuild.
        conn.execute_batch("DROP INDEX IF EXISTS hnsw_blocks_vec;")?;
        let tx = conn.transaction()?;
        match on_collision {
            OnCollision::Replace => {
                // Delete the colliding instances, then insert ALL source rows.
                tx.execute_batch(&format!(
                    "DELETE FROM blocks WHERE page_id IN (SELECT page_id FROM {alias}.pages); \
                     DELETE FROM links  WHERE src_page IN (SELECT page_id FROM {alias}.pages); \
                     DELETE FROM pages  WHERE page_id IN (SELECT page_id FROM {alias}.pages); \
                     INSERT INTO pages  BY NAME SELECT * FROM {alias}.pages; \
                     INSERT INTO links  BY NAME SELECT * FROM {alias}.links; \
                     INSERT INTO blocks BY NAME SELECT * FROM {alias}.blocks;"
                ))?;
            }
            OnCollision::Skip | OnCollision::Error => {
                // Import only new page_ids. Snapshot the pre-existing ids so
                // pages/links/blocks all filter against the SAME set (pages is
                // inserted first, which would otherwise shift the filter).
                // CREATE OR REPLACE so a `_existing` left over from a merge that
                // crashed between CREATE and DROP on this long-lived connection
                // doesn't wedge every subsequent merge.
                tx.execute_batch(&format!(
                    "CREATE OR REPLACE TEMP TABLE _existing AS SELECT page_id FROM pages; \
                     INSERT INTO pages  BY NAME SELECT * FROM {alias}.pages  \
                       WHERE page_id  NOT IN (SELECT page_id FROM _existing); \
                     INSERT INTO links  BY NAME SELECT * FROM {alias}.links  \
                       WHERE src_page NOT IN (SELECT page_id FROM _existing); \
                     INSERT INTO blocks BY NAME SELECT * FROM {alias}.blocks \
                       WHERE page_id  NOT IN (SELECT page_id FROM _existing); \
                     DROP TABLE _existing;"
                ))?;
            }
        }
        tx.commit()?;

        // Rebuild the vector index (same DDL as the schema), then drop the lock
        // before refreshing FTS (which re-locks the connection).
        conn.execute_batch(
            "CREATE INDEX hnsw_blocks_vec ON blocks USING HNSW (dense_vec) \
             WITH (metric = 'cosine', ef_construction = 128, ef_search = 64, M = 16);",
        )?;
        drop(conn);
        self.refresh_fts().await?;
        self.bump_mutation_epoch();

        Ok(MergeReport {
            source_pages: source_pages as usize,
            collisions: collisions as usize,
        })
    }

    /// Bulk-load this (FRESH, empty) indexer from an already-attached
    /// DuckLake catalog (`alias`) — the reader half of the DuckLake
    /// snapshot cycle (`snapshot::adopt_lake`), borrowing
    /// [`Self::merge_from_attached`]'s fast-path shape: rows copy
    /// DuckDB→DuckDB with `INSERT … BY NAME` (no re-embedding), HNSW is
    /// (re)built after the load via [`Self::reindex_vectors`], the BM25
    /// FTS snapshot refreshes once at the end.
    ///
    /// Differences from `merge_from_attached`, both lake-shaped:
    /// - `blocks.dense_vec` is cast back `::FLOAT[768]` — the lake
    ///   carries the `FLOAT[]` *list* type because DuckLake rejects the
    ///   fixed-width array (spike note 2026-07-17) — and the cast runs
    ///   BEFORE the HNSW build so the index is over the array type;
    /// - the registry tables (`group_members`, `external_endpoints`,
    ///   `pack_subscriptions`) come along (`external_credentials` is
    ///   never in a lake; chat/CRDT/events stay empty — Phase B);
    /// - no collision handling: the target is a freshly migrated
    ///   in-memory DB, so every source row is new by construction.
    ///
    /// Returns the lake snapshot id observed by the SAME transaction
    /// that copied the rows, so the id names exactly the state that was
    /// loaded even if a writer publishes mid-adopt.
    pub(crate) async fn load_from_lake(&self, alias: &str) -> Result<i64, IndexerError> {
        if !is_valid_attach_alias(alias) {
            return Err(IndexerError::InvalidExternalSource {
                reason: "attach alias is not a valid identifier",
            });
        }
        let snapshot_id: i64 = {
            let mut conn = self.conn.lock().await;
            // Per-row HNSW maintenance is slow; drop, bulk-insert, rebuild.
            conn.execute_batch("DROP INDEX IF EXISTS hnsw_blocks_vec;")?;
            let tx = conn.transaction()?;
            // Read the snapshot id INSIDE the copy transaction: DuckLake
            // gives snapshot-consistent reads per transaction, so this id
            // is the one every SELECT below serves.
            let snapshot_id: i64 = tx.query_row(
                &format!("SELECT max(snapshot_id) FROM ducklake_snapshots('{alias}')"),
                [],
                |r| r.get(0),
            )?;
            tx.execute_batch(&format!(
                "INSERT INTO pages  BY NAME SELECT * FROM {alias}.pages; \
                 INSERT INTO links  BY NAME SELECT * FROM {alias}.links; \
                 INSERT INTO group_members      BY NAME SELECT * FROM {alias}.group_members; \
                 INSERT INTO external_endpoints BY NAME SELECT * FROM {alias}.external_endpoints; \
                 INSERT INTO pack_subscriptions BY NAME SELECT * FROM {alias}.pack_subscriptions; \
                 INSERT INTO blocks BY NAME SELECT * \
                   REPLACE (dense_vec::FLOAT[{BLOCKS_DENSE_VEC_DIM}] AS dense_vec) \
                   FROM {alias}.blocks;"
            ))?;
            tx.commit()?;
            snapshot_id
        };
        // HNSW after the load (the in-memory reader DB needs no
        // experimental-persistence flag), then one FTS refresh. Both
        // re-take the connection lock, so the guard above must be gone.
        self.reindex_vectors().await?;
        self.refresh_fts().await?;
        Ok(snapshot_id)
    }

    /// Read an inbox blob by id (delegates to the LaneStore). Used by the
    /// document ingest worker, which only holds an `Arc<Indexer>`.
    pub async fn read_inbox_blob(
        &self,
        id: &escurel_storage::BlobId,
    ) -> Result<Bytes, IndexerError> {
        Ok(self.store.get_inbox_blob(self.tenant.as_str(), id).await?)
    }

    /// Read a canonical blob by id (delegates to the LaneStore).
    pub async fn read_blob(&self, id: &escurel_storage::BlobId) -> Result<Bytes, IndexerError> {
        Ok(self.store.get_blob(self.tenant.as_str(), id).await?)
    }

    /// Promote an inbox blob to the canonical area after a successful
    /// materialise (delegates to the LaneStore).
    pub async fn promote_blob(&self, id: &escurel_storage::BlobId) -> Result<(), IndexerError> {
        Ok(self
            .store
            .promote_inbox_blob(self.tenant.as_str(), id)
            .await?)
    }

    /// Idempotently ensure the mandatory `escurel` meta-skill page is
    /// present and indexed. No-op when a skill page named `escurel`
    /// already exists — operators may ship their own extended version
    /// (with appended sections), and re-opening an existing tenant
    /// must not clobber it.
    ///
    /// Called at indexer open (binary boot and the test harness) so
    /// every *served* tenant exposes the navigation doc the agent
    /// contract promises (`docs/contract/agent-interface.md` locked
    /// decision 3). Writes the canonical markdown to the LaneStore
    /// (keeping `audit` clean) and indexes it.
    pub async fn ensure_meta_skill(&self) -> Result<(), IndexerError> {
        {
            let conn = self.conn.lock().await;
            let n: i64 = conn.query_row(
                "SELECT count(*) FROM pages WHERE page_type = 'skill' AND slug = ?",
                params![crate::meta_skill::META_SKILL_ID],
                |row| row.get(0),
            )?;
            if n > 0 {
                return Ok(());
            }
        }
        let key = Key::new(
            self.tenant.as_str(),
            crate::meta_skill::META_SKILL_PAGE_ID.to_owned(),
        )?;
        self.store
            .write(
                &key,
                Bytes::from_static(crate::meta_skill::META_SKILL_MD.as_bytes()),
            )
            .await?;
        self.update_page(
            crate::meta_skill::META_SKILL_PAGE_ID,
            crate::meta_skill::META_SKILL_MD,
        )
        .await?;
        // Make the new block searchable (FTS has no incremental refresh).
        self.refresh_fts().await?;
        Ok(())
    }

    /// Compare markdown on the LaneStore (under `markdown/`) with
    /// page rows in the DuckDB `pages` table; return the two-way diff.
    pub async fn audit(&self) -> Result<AuditDrift, IndexerError> {
        let on_disk = self.list_markdown_paths().await?;
        let in_db = self.list_indexed_page_ids().await?;

        let mut drift = AuditDrift {
            markdown_not_in_duckdb: on_disk.difference(&in_db).cloned().collect(),
            indexed_but_no_markdown: in_db.difference(&on_disk).cloned().collect(),
        };
        drift.markdown_not_in_duckdb.sort();
        drift.indexed_but_no_markdown.sort();
        Ok(drift)
    }

    /// Re-run [`Self::update_page`] for every markdown file the
    /// LaneStore holds for this tenant. Used to recover from a lost
    /// or corrupted DuckDB file — canonical markdown is the source
    /// of truth, so any rows whose backing markdown is gone must
    /// also vanish from the index. We truncate the three tables in
    /// one transaction before re-upserting, so the operation is
    /// "drop the index, recreate from markdown."
    pub async fn rebuild(&self) -> Result<(), IndexerError> {
        self.rebuild_with_progress(|_| {}).await
    }

    /// Like [`Self::rebuild`], but invokes `on_progress` once per
    /// page reindexed with the running `(done, total, page_id)`
    /// tuple. Used by the `rebuild` admin tool to stream
    /// `RebuildProgress` chunks to the caller. `done` is `1` on
    /// the first emission and equal to `total` on the last.
    pub async fn rebuild_with_progress<F>(&self, mut on_progress: F) -> Result<(), IndexerError>
    where
        F: FnMut(RebuildProgress<'_>),
    {
        let on_disk = self.list_markdown_paths().await?;
        let mut sorted: Vec<String> = on_disk.into_iter().collect();
        // Sort so the progress stream is deterministic; callers
        // that compare chunk lists across runs (tests, audit
        // tooling) rely on this.
        sorted.sort();
        let total = sorted.len() as u64;

        {
            let mut conn = self.conn.lock().await;
            let tx = conn.transaction()?;
            tx.execute("DELETE FROM blocks", [])?;
            tx.execute("DELETE FROM links", [])?;
            tx.execute("DELETE FROM pages", [])?;
            tx.commit()?;
        }

        for (idx, path) in sorted.into_iter().enumerate() {
            let key = Key::new(self.tenant.as_str(), path.clone())?;
            let body = self.store.read(&key).await?;
            let content = std::str::from_utf8(&body).map_err(|_| IndexerError::NotUtf8 {
                page_id: path.clone(),
            })?;
            self.update_page(&path, content).await?;
            on_progress(RebuildProgress {
                done: (idx as u64) + 1,
                total,
                current_page: &path,
            });
        }
        // SQL views are external — no data to rebuild, but the view objects
        // must be reconstructed from each overlay's backend_ref.source so a
        // from-scratch rebuild yields a queryable index (REQ-NF-01).
        self.rebuild_sql_views().await?;
        // Document chunks are derived from the retained blob: re-extract +
        // re-chunk + re-embed + re-index, replacing the single overlay block
        // the main loop wrote with the correct chunk-blocks (REQ-NF-01).
        crate::backend::document::rebuild_documents(self).await?;
        // Reclaim canonical blobs no overlay references — dead weight from a
        // materialise that failed after promotion, or a deleted instance
        // (REQ-NF-02). Runs after the overlays are re-indexed so the
        // referenced-set is authoritative. Inbox blobs are retained.
        crate::backend::document::reclaim_orphan_blobs(self).await?;
        // The truncate itself is a mutation even when zero pages were
        // re-indexed (the per-page `update_page` calls above bump too —
        // the counter is monotone, only equality vs. last-published
        // matters, so over-counting is harmless).
        self.bump_mutation_epoch();
        Ok(())
    }

    /// Document-side audit reconciliation (REQ-NF-02): document overlays
    /// whose canonical blob is missing/invalid, plus orphan blobs no overlay
    /// references. `(page_id_or_blob_id, reason)` each.
    pub async fn audit_documents(&self) -> Result<Vec<(String, String)>, IndexerError> {
        crate::backend::document::audit_documents(self).await
    }

    /// Seed the tenant from an external directory of markdown files
    /// (e.g. `examples/crm-demo`). For each `*.md` found recursively:
    /// write it into the canonical LaneStore under
    /// `markdown/<relpath>` and index it via [`Self::update_page`],
    /// skills first so wikilink targets are present, then refresh the
    /// FTS index over the populated blocks. Returns the number of
    /// files seeded.
    ///
    /// Idempotent: re-seeding the same content upserts in place (same
    /// `body_hash`), leaving no drift. Distinct from [`Self::rebuild`],
    /// which re-indexes markdown the LaneStore *already* holds —
    /// `seed_from_dir` *imports* markdown from outside the tenant lane.
    /// The page_id equals the lane key (`markdown/<relpath>`) so
    /// [`Self::audit`] stays clean.
    pub async fn seed_from_dir(&self, dir: &Path) -> Result<usize, IndexerError> {
        let mut files: Vec<(String, String)> = Vec::new();
        collect_md(dir, dir, &mut files)?;
        // Skills before instances so links resolve at index time;
        // stable path order within each group for deterministic seeds.
        files.sort_by(|a, b| (!is_skill(&a.1), a.0.as_str()).cmp(&(!is_skill(&b.1), b.0.as_str())));

        for (relpath, content) in &files {
            let page_id = format!("markdown/{relpath}");
            let key = Key::new(self.tenant.as_str(), page_id.clone())?;
            self.store.write(&key, Bytes::from(content.clone())).await?;
            self.update_page(&page_id, content).await?;
        }
        // FTS has no incremental refresh PRAGMA; rebuild it over the
        // now-populated blocks (see search.rs / discovered notes).
        self.refresh_fts().await?;

        // M7: optional `events.json` (events into the inbox, optionally
        // assigned to an instance) and `history.json` (CRDT snapshot
        // timelines) alongside the markdown pages.
        self.seed_events_file(dir).await?;
        self.seed_history_file(dir).await?;

        Ok(files.len())
    }

    /// Load `<dir>/events.json` if present: an array of events captured
    /// into the inbox. An event with `status: "processed"` and an
    /// `instance` is also assigned (enters that instance's event
    /// history); otherwise it stays in the inbox (an `instance` then
    /// only pre-flags a candidate).
    async fn seed_events_file(&self, dir: &Path) -> Result<(), IndexerError> {
        let path = dir.join("events.json");
        let raw = match std::fs::read_to_string(&path) {
            Ok(s) => s,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(source) => {
                return Err(IndexerError::SeedIo {
                    path: path.display().to_string(),
                    source,
                });
            }
        };
        let events: Vec<SeedEvent> = serde_json::from_str(&raw)?;
        // Idempotent bootstrap: skip if the store already holds events
        // (a re-seed, or live captures already happened).
        {
            let conn = self.conn.lock().await;
            let n: i64 = conn.query_row("SELECT count(*) FROM events", [], |r| r.get(0))?;
            if n > 0 {
                return Ok(());
            }
        }
        for e in events {
            let processed = e.status.as_deref() == Some("processed");
            let pre_flag = if processed { None } else { e.instance.clone() };
            let stored = self
                .capture_event(crate::events::NewEvent {
                    event_id: e.event_id,
                    at: e.at,
                    source: e.source,
                    mime: e.mime,
                    label_skill: e.label_skill,
                    instance_page_id: pre_flag,
                    title: e.title,
                    body: e.body,
                    provenance: e.provenance,
                })
                .await?;
            if processed && let Some(inst) = e.instance {
                self.assign_event(&stored.event_id, &inst).await?;
            }
        }
        Ok(())
    }

    /// Load `<dir>/history.json` if present: an array of per-page CRDT
    /// snapshot timelines seeded via [`Self::seed_snapshot_history`].
    async fn seed_history_file(&self, dir: &Path) -> Result<(), IndexerError> {
        let path = dir.join("history.json");
        let raw = match std::fs::read_to_string(&path) {
            Ok(s) => s,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(source) => {
                return Err(IndexerError::SeedIo {
                    path: path.display().to_string(),
                    source,
                });
            }
        };
        let histories: Vec<SeedHistory> = serde_json::from_str(&raw)?;
        // Idempotent bootstrap: skip if any snapshots already exist.
        {
            let conn = self.conn.lock().await;
            let n: i64 = conn.query_row("SELECT count(*) FROM crdt_snapshots", [], |r| r.get(0))?;
            if n > 0 {
                return Ok(());
            }
        }
        for h in histories {
            let states: Vec<(&str, &str)> = h
                .states
                .iter()
                .map(|s| (s.taken_at.as_str(), s.markdown.as_str()))
                .collect();
            self.seed_snapshot_history(&h.page_id, &states).await?;
        }
        Ok(())
    }

    async fn list_markdown_paths(&self) -> Result<HashSet<String>, IndexerError> {
        let prefix = Key::new(self.tenant.as_str(), "markdown/")?;
        let keys = self.store.list(&prefix).await?;
        Ok(keys.into_iter().map(|k| k.path().to_owned()).collect())
    }

    async fn list_indexed_page_ids(&self) -> Result<HashSet<String>, IndexerError> {
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare("SELECT page_id FROM pages")?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        let mut out = HashSet::new();
        for row in rows {
            out.insert(row?);
        }
        Ok(out)
    }

    /// Attach an external read-only DuckDB catalog onto this
    /// indexer's live connection so `[[query::*]]` stored queries
    /// (and any `[[table::ext.*]]` surface built on them) can read
    /// it via `<alias>.<table>`.
    ///
    /// Uses DuckDB's *native* `ATTACH` (no DuckLake extension for
    /// v1): `ATTACH '<source>' AS <alias> (READ_ONLY)`. The catalog
    /// is attached read-only — escurel never writes through an
    /// external lane.
    ///
    /// ## Injection defence
    ///
    /// DuckDB does not support parameter binding for `ATTACH`
    /// path/alias positions, so both are spliced into the SQL as
    /// literals. `attach_external` is admin-only, but we still
    /// validate strictly: `alias` is constrained to
    /// `[A-Za-z0-9_]+` by the caller (it is derived, not
    /// user-supplied), and `source` is rejected if it contains any
    /// character that could break out of the single-quoted string
    /// literal or stack a second statement (quotes, backslashes,
    /// semicolons, control characters). Callers should pre-validate
    /// via [`sanitize_attach_source`] / [`derive_attach_alias`];
    /// this method re-checks defensively so a future caller can't
    /// regress the boundary.
    ///
    /// # Errors
    ///
    /// Returns [`IndexerError::InvalidExternalSource`] when `source`
    /// or `alias` fails validation, and [`IndexerError::Duckdb`]
    /// when DuckDB rejects the attach (e.g. the file is not a
    /// readable database).
    pub async fn attach_external(&self, alias: &str, source: &str) -> Result<(), IndexerError> {
        if !is_valid_attach_alias(alias) {
            return Err(IndexerError::InvalidExternalSource {
                reason: "derived alias must be a non-empty [A-Za-z0-9_] identifier",
            });
        }
        if !is_safe_attach_source(source) {
            return Err(IndexerError::InvalidExternalSource {
                reason: "source path/uri contains an unsafe character \
                         (quote, backslash, semicolon, or control char)",
            });
        }
        let sql = format!("ATTACH '{source}' AS {alias} (READ_ONLY)");
        let conn = self.conn.lock().await;
        conn.execute_batch(&sql)?;
        Ok(())
    }
}

/// Derive a DuckDB catalog alias from an external `source` path/uri:
/// the file stem (last path segment, sans extension), lower-cased,
/// with any non-`[A-Za-z0-9_]` run collapsed to a single `_`.
///
/// Returns `None` when nothing usable can be derived (empty source,
/// or a stem that is all separators).
#[must_use]
pub fn derive_attach_alias(source: &str) -> Option<String> {
    // Last path segment (works for both `/` paths and bare names;
    // `s3://bucket/key.duckdb` keys also split on `/`).
    let last = source.rsplit(['/', '\\']).next().unwrap_or(source).trim();
    // Drop a single trailing extension if present.
    let stem = last.rsplit_once('.').map_or(last, |(s, _ext)| s);
    let mut out = String::with_capacity(stem.len());
    let mut prev_us = false;
    for ch in stem.chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' {
            out.push(ch);
            prev_us = false;
        } else if !prev_us && !out.is_empty() {
            out.push('_');
            prev_us = true;
        }
    }
    while out.ends_with('_') {
        out.pop();
    }
    if out.is_empty()
        || !out
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
    {
        // DuckDB identifiers must not start with a digit when used
        // unquoted; prefix when needed rather than failing outright.
        if out.is_empty() {
            return None;
        }
        return Some(format!("ext_{out}"));
    }
    Some(out)
}

/// Whether `alias` is a safe unquoted DuckDB identifier to splice
/// into the `ATTACH ... AS <alias>` position.
#[must_use]
pub fn is_valid_attach_alias(alias: &str) -> bool {
    !alias.is_empty()
        && alias
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && alias.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Whether `source` is safe to splice into a single-quoted SQL
/// string literal in the `ATTACH '<source>'` position. Rejects any
/// quote, backslash, semicolon, or control character — the
/// characters that could close the literal, stack a statement, or
/// smuggle an escape.
#[must_use]
pub fn is_safe_attach_source(source: &str) -> bool {
    !source.is_empty()
        && !source
            .chars()
            .any(|c| c == '\'' || c == '"' || c == '\\' || c == ';' || c == '`' || c.is_control())
}

/// Recursively collect `(relpath, content)` for every `*.md` under
/// `root`. `relpath` is `dir`-relative with forward slashes (the lane
/// key convention).
fn collect_md(
    root: &Path,
    dir: &Path,
    out: &mut Vec<(String, String)>,
) -> Result<(), IndexerError> {
    let entries = std::fs::read_dir(dir).map_err(|source| IndexerError::SeedIo {
        path: dir.display().to_string(),
        source,
    })?;
    for entry in entries {
        let entry = entry.map_err(|source| IndexerError::SeedIo {
            path: dir.display().to_string(),
            source,
        })?;
        let path = entry.path();
        if path.is_dir() {
            collect_md(root, &path, out)?;
        } else if path.extension().and_then(|e| e.to_str()) == Some("md") {
            let content =
                std::fs::read_to_string(&path).map_err(|source| IndexerError::SeedIo {
                    path: path.display().to_string(),
                    source,
                })?;
            // Skip non-page markdown (e.g. a corpus README): an escurel
            // page always opens with a `---` frontmatter fence.
            if !content.trim_start().starts_with("---") {
                continue;
            }
            let rel = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .to_string_lossy()
                .replace('\\', "/");
            out.push((rel, content));
        }
    }
    Ok(())
}

/// True if the markdown declares `type: skill` in its frontmatter.
/// Cheap scan of the leading lines — enough to order skills before
/// instances during a seed.
fn is_skill(content: &str) -> bool {
    content.lines().take(40).any(|l| l.trim() == "type: skill")
}

fn hash_body(content: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(content.as_bytes());
    hex(&hasher.finalize())
}

fn hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0xf) as usize] as char);
    }
    out
}

/// One entry in a seed `events.json` array (M7).
#[derive(serde::Deserialize)]
struct SeedEvent {
    #[serde(default)]
    event_id: Option<String>,
    #[serde(default)]
    at: Option<String>,
    #[serde(default)]
    source: String,
    #[serde(default)]
    mime: String,
    #[serde(default)]
    label_skill: String,
    /// The instance this event is about (assigned when `status` is
    /// `"processed"`, else a candidate pre-flag).
    #[serde(default)]
    instance: Option<String>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    title: String,
    #[serde(default)]
    body: String,
    #[serde(default)]
    provenance: Option<serde_json::Value>,
}

/// One per-page snapshot timeline in a seed `history.json` array (M7).
#[derive(serde::Deserialize)]
struct SeedHistory {
    page_id: String,
    states: Vec<SeedHistoryState>,
}

#[derive(serde::Deserialize)]
struct SeedHistoryState {
    taken_at: String,
    markdown: String,
}

/// Extract `[[skill::id]]` wikilinks from every frontmatter field value,
/// returning each with the field key it came from. YAML parses an
/// unquoted `about: [[engagement::spine]]` as a nested flow sequence, so
/// each value is rendered back to its raw markup (`[[engagement::spine]]`)
/// and run through the same parser used on the body.
fn frontmatter_wikilinks(
    fields: &escurel_md::YamlMapping,
) -> Vec<(String, escurel_md::wikilink::WikilinkParsed)> {
    let mut out = Vec::new();
    for (k, v) in fields {
        let Some(key) = k.as_str() else { continue };
        let markup = render_yaml_markup(v);
        if !markup.contains("[[") {
            continue;
        }
        for wl in parse_wikilinks(&markup) {
            out.push((key.to_owned(), wl));
        }
    }
    out
}

/// Render a YAML value to a string that preserves `[[skill::id]]` markup:
/// a sequence becomes `[a, b]` (so a nested flow sequence reconstructs
/// `[[…]]`), scalars render raw (no quotes), mappings render their values.
fn render_yaml_markup(v: &escurel_md::YamlValue) -> String {
    use escurel_md::YamlValue as Y;
    match v {
        Y::String(s) => s.clone(),
        Y::Number(n) => n.to_string(),
        Y::Bool(b) => b.to_string(),
        Y::Null => String::new(),
        Y::Sequence(items) => {
            let inner: Vec<String> = items.iter().map(render_yaml_markup).collect();
            format!("[{}]", inner.join(", "))
        }
        Y::Mapping(m) => m
            .values()
            .map(render_yaml_markup)
            .collect::<Vec<_>>()
            .join(", "),
        Y::Tagged(t) => render_yaml_markup(&t.value),
    }
}

/// Convert a YAML mapping into a JSON string for the `pages.frontmatter`
/// column. DuckDB's JSON type accepts any well-formed JSON text.
pub(crate) fn mapping_to_json(mapping: &escurel_md::YamlMapping) -> Result<String, IndexerError> {
    let value = escurel_md::YamlValue::Mapping(mapping.clone());
    let json = serde_json::to_string(&value)?;
    Ok(json)
}

/// Format a Vec<f32> as a DuckDB array literal `[x,y,z,...]`.
///
/// Safe to splice into SQL via `format!` — the values are `f32`s
/// rendered with `Display`, so no input strings reach the
/// statement (no injection surface). Used by the blocks insert,
/// because duckdb-rs's `params!` doesn't have a direct binding
/// for fixed-size float arrays.
pub(crate) fn format_vector_literal(v: &[f32]) -> String {
    let mut out = String::with_capacity(v.len() * 8 + 2);
    out.push('[');
    for (i, x) in v.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(&format!("{x}"));
    }
    out.push(']');
    out
}
