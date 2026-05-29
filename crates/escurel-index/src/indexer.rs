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

use bytes::Bytes;
use duckdb::{Connection, params};
use escurel_embed::{EmbedError, Embedder};
use escurel_md::wikilink::parse_wikilinks;
use escurel_md::{PageType, parse};
use escurel_storage::{Key, LaneStore};
use sha2::{Digest, Sha256};
use thiserror::Error;
use tokio::sync::Mutex;

// Re-export the chat-history surface so consumers and tests can
// import the input/output types from the same module path as
// `Indexer` itself.
pub use crate::chat::{AppendChatMessage, ChatMessage, ChatPage, ListChatMessages};

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
    tenant: String,
}

/// Per-page progress event emitted by
/// [`Indexer::rebuild_with_progress`]. Borrowed so the callback
/// can receive a `&str` without forcing an allocation per page;
/// gRPC handlers copy it into the proto message at the boundary.
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
    #[error("seed io error at {path}: {source}")]
    SeedIo {
        path: String,
        #[source]
        source: std::io::Error,
    },
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
            tenant: tenant.into(),
        })
    }

    /// Tenant id this indexer was bound to at construction.
    #[must_use]
    pub fn tenant(&self) -> &str {
        &self.tenant
    }

    /// Upsert the page identified by `page_id` from the markdown
    /// `content` blob, inside a single DuckDB transaction.
    ///
    /// `page_id` is the caller's stable handle for this page —
    /// during bootstrap we use the markdown file's relative path
    /// within the tenant (e.g. `markdown/skills/customer.md`).
    /// ULID + slug semantics arrive in a later PR.
    pub async fn update_page(&self, page_id: &str, content: &str) -> Result<(), IndexerError> {
        let parsed = parse(content)?;
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

        // Take the per-tenant DuckDB mutex BEFORE embedding. A
        // codex review of M2.1 caught that the obvious "embed
        // outside the lock" optimisation lets two concurrent
        // `update_page` calls for the same `page_id` commit out
        // of order: the slower embed finishes second and
        // overwrites the newer content. Production avoids this
        // via the spec's per-tenant write-RwLock in `kb-server`
        // (`docs/spec/platform.md §Concurrency`); the M2-stage
        // Indexer's single connection mutex is the only barrier
        // and must serialise the whole `embed → write`
        // sequence. See
        // `docs/notes/discovered/2026-05-24-update-page-embed-order.md`.
        let mut conn = self.conn.lock().await;

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
    /// tuple. Used by `EscurelAdmin.Rebuild` to stream
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
        Ok(())
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
        Ok(files.len())
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

/// Convert a YAML mapping into a JSON string for the `pages.frontmatter`
/// column. DuckDB's JSON type accepts any well-formed JSON text.
fn mapping_to_json(mapping: &escurel_md::YamlMapping) -> Result<String, IndexerError> {
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
