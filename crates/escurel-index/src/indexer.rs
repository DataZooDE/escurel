//! Per-tenant indexer: parses markdown, upserts into DuckDB,
//! audits drift against the canonical markdown on the LaneStore,
//! and rebuilds the DuckDB store from canonical markdown.
//!
//! All write paths run inside a single DuckDB transaction so a
//! mid-write SIGKILL leaves the pages / links / blocks tables
//! atomically rolled back, matching the spec README's failure model.

use std::collections::HashSet;
use std::sync::Arc;

use duckdb::{Connection, params};
use escurel_embed::{EmbedError, Embedder};
use escurel_md::wikilink::parse_wikilinks;
use escurel_md::{PageType, parse};
use escurel_storage::{Key, LaneStore};
use sha2::{Digest, Sha256};
use thiserror::Error;
use tokio::sync::Mutex;

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
    embedder: Arc<dyn Embedder>,
    pub(crate) conn: Mutex<Connection>,
    tenant: String,
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
             (page_id, slug, skill, page_type, frontmatter, body_hash, at_ts, created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?::JSON, ?, \
                     TRY_CAST(? AS TIMESTAMP), CURRENT_TIMESTAMP, CURRENT_TIMESTAMP)",
            params![
                page_id,
                slug,
                skill,
                page_type_str,
                frontmatter_json,
                body_hash,
                at_ts,
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
        let dense_vec_literal = format!(
            "{vec}::FLOAT[{dim}]",
            vec = dense_vec_sql,
            dim = BLOCKS_DENSE_VEC_DIM,
        );
        let block_insert_sql = format!(
            "INSERT INTO blocks \
             (block_id, page_id, anchor, ordinal, body, dense_vec, skill, page_type, at_ts) \
             VALUES (?, ?, 'blk-0', 0, ?, {dense_vec_literal}, ?, ?, TRY_CAST(? AS TIMESTAMP))",
        );
        tx.execute(
            &block_insert_sql,
            params![block_id, page_id, body_text, skill, page_type_str, at_ts],
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
        let on_disk = self.list_markdown_paths().await?;

        {
            let mut conn = self.conn.lock().await;
            let tx = conn.transaction()?;
            tx.execute("DELETE FROM blocks", [])?;
            tx.execute("DELETE FROM links", [])?;
            tx.execute("DELETE FROM pages", [])?;
            tx.commit()?;
        }

        for path in on_disk {
            let key = Key::new(self.tenant.as_str(), path.clone())?;
            let body = self.store.read(&key).await?;
            let content = std::str::from_utf8(&body).map_err(|_| IndexerError::NotUtf8 {
                page_id: path.clone(),
            })?;
            self.update_page(&path, content).await?;
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
fn format_vector_literal(v: &[f32]) -> String {
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
