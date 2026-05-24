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
use escurel_md::wikilink::parse_wikilinks;
use escurel_md::{PageType, parse};
use escurel_storage::{Key, LaneStore};
use sha2::{Digest, Sha256};
use thiserror::Error;
use tokio::sync::Mutex;

/// Per-tenant indexer.
///
/// Holds an open DuckDB connection plus a handle on the canonical
/// markdown lane (any [`LaneStore`] impl). The connection is wrapped
/// in a `tokio::sync::Mutex` because DuckDB connections are
/// single-threaded; concurrent async callers serialise through it.
pub struct Indexer {
    store: Arc<dyn LaneStore>,
    conn: Mutex<Connection>,
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
}

impl Indexer {
    pub fn new(store: Arc<dyn LaneStore>, conn: Connection, tenant: impl Into<String>) -> Self {
        Self {
            store,
            conn: Mutex::new(conn),
            tenant: tenant.into(),
        }
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
        let body_text = parsed.body.to_owned();
        let wikilinks = parse_wikilinks(&body_text);

        let mut conn = self.conn.lock().await;
        let tx = conn.transaction()?;

        // pages: upsert via DELETE + INSERT to keep semantics
        // straightforward without depending on an ON CONFLICT clause
        // that varies by DuckDB version.
        tx.execute("DELETE FROM pages WHERE page_id = ?", params![page_id])?;
        tx.execute(
            "INSERT INTO pages \
             (page_id, slug, skill, page_type, frontmatter, body_hash, at_ts, created_at, updated_at) \
             VALUES (?, NULL, ?, ?, ?::JSON, ?, \
                     TRY_CAST(? AS TIMESTAMP), CURRENT_TIMESTAMP, CURRENT_TIMESTAMP)",
            params![page_id, skill, page_type_str, frontmatter_json, body_hash, at_ts],
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
                 VALUES (?, '', NULL, ?, NULL, ?, ?)",
                params![page_id, dst_page, link_skill, wl.version.as_deref()],
            )?;
        }

        // blocks: single block per page for now (whole body). Block-
        // anchor splitting + per-block embeddings land in M2.
        tx.execute("DELETE FROM blocks WHERE page_id = ?", params![page_id])?;
        let block_id = format!("{page_id}:blk-0");
        tx.execute(
            "INSERT INTO blocks \
             (block_id, page_id, anchor, ordinal, body, dense_vec, skill, page_type, at_ts) \
             VALUES (?, ?, 'blk-0', 0, ?, NULL, ?, ?, TRY_CAST(? AS TIMESTAMP))",
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
    /// of truth.
    pub async fn rebuild(&self) -> Result<(), IndexerError> {
        let on_disk = self.list_markdown_paths().await?;
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
