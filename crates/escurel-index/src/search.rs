//! Hybrid search across `blocks` — vector + FTS, RRF-fused.
//!
//! ## What this does
//!
//! 1. Embed the query with the indexer's configured `Embedder`.
//! 2. Pull the top-N vector-distance candidates via the `vss`
//!    HNSW index on `blocks.dense_vec`.
//! 3. Pull the top-N BM25 candidates via the `fts` extension on
//!    `blocks.body`.
//! 4. Combine the two rankings via **Reciprocal Rank Fusion**
//!    (`score = Σ 1 / (k_rrf + rank)`, `k_rrf = 60`).
//! 5. Hydrate the top-k fused hits into [`SearchHit`]s by joining
//!    against `pages`.
//!
//! ## FTS freshness
//!
//! The `fts` extension has no incremental refresh
//! (`docs/notes/discovered/2026-05-24-fts-no-refresh-pragma.md`):
//! the BM25 index built at migration time is empty until the
//! caller invokes [`Indexer::refresh_fts`]. This is intentional
//! — every refresh is O(rows), so per-write rebuild is
//! unacceptable; callers (and tests) refresh deliberately after
//! a batch of writes.
//!
//! ## Granularity + post-filter
//!
//! - `granularity = page` collapses the fused ranking to one hit per
//!   page (best-scoring block wins; the block `anchor` is dropped).
//!   `block` (default) returns block hits unchanged.
//! - An optional frontmatter `filter` ([`crate::filter`]) is applied
//!   to each candidate's frontmatter *after* retrieval, before the
//!   top-`k` cut, so a filtered search still returns up to `k` hits.
//!
//! ## What does NOT ship here
//!
//! - The ADR-0001 retrieval-quality gate. That gate requires a
//!   real embedder; M2.2 (EmbeddingGemma) lands the embedder,
//!   then the gate is the first thing to run before production.

use std::collections::{HashMap, HashSet};

use escurel_md::PageType;

use crate::indexer::BLOCKS_DENSE_VEC_DIM;
use crate::{Indexer, IndexerError};

/// Result granularity for [`Indexer::search_with`]. `Block` returns one
/// hit per matching block; `Page` collapses adjacent block hits to one
/// per page (the best-scoring block wins, its `anchor` dropped).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Granularity {
    #[default]
    Block,
    Page,
}

impl Granularity {
    /// Parse the wire value (`"page"` → `Page`, anything else →
    /// `Block`). Empty / unknown defaults to `Block`, matching the
    /// protocol default.
    #[must_use]
    pub fn from_arg(s: &str) -> Self {
        if s.eq_ignore_ascii_case("page") {
            Self::Page
        } else {
            Self::Block
        }
    }

    /// Wire string reported back in the search response.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Block => "block",
            Self::Page => "page",
        }
    }
}

/// One block-granularity hit. Shape mirrors `Hit` in
/// `docs/spec/protocol.md §Shared types`.
#[derive(Debug, Clone, PartialEq)]
pub struct SearchHit {
    pub page_id: String,
    pub slug: Option<String>,
    pub skill: String,
    pub page_type: PageType,
    pub anchor: Option<String>,
    pub snippet: String,
    /// RRF-fused score.
    pub score: f64,
    /// Frontmatter projected as a JSON object.
    pub frontmatter_excerpt: serde_json::Value,
}

/// Reciprocal Rank Fusion constant. 60 is the canonical default;
/// later PRs may make this tunable.
const K_RRF: f64 = 60.0;

/// How many vector / FTS candidates to retrieve before fusing.
/// Sized at `4 × k` (capped at 200) — empirically enough for the
/// two rankings to overlap on the high-quality hits without
/// dragging in noise.
fn candidate_pool(k: usize) -> usize {
    (k.saturating_mul(4)).clamp(20, 200)
}

impl Indexer {
    /// Rebuild the FTS index over the current `blocks` rows.
    ///
    /// The fts extension has no incremental refresh; callers
    /// must rebuild after a batch of writes so [`Self::search`]
    /// sees new bodies. Production callers typically trigger
    /// this from a debounce / compact job
    /// (`docs/notes/discovered/2026-05-24-fts-no-refresh-pragma.md`);
    /// tests call it directly between seeding and search.
    pub async fn refresh_fts(&self) -> Result<(), IndexerError> {
        let conn = self.conn.lock().await;
        conn.execute_batch(
            "PRAGMA create_fts_index('blocks', 'block_id', 'body', \
             stemmer = 'porter', stopwords = 'english', \
             ignore = '(\\.|[^a-z])+', lower = 1, overwrite = 1);",
        )?;
        Ok(())
    }

    /// Hybrid block search with default granularity (`Block`) and no
    /// frontmatter post-filter. Thin wrapper over [`Self::search_with`]
    /// kept for the many internal callers that don't need the extra
    /// knobs.
    pub async fn search(
        &self,
        q: &str,
        k: usize,
        page_type: Option<PageType>,
        skill: Option<&str>,
        as_of: Option<&str>,
        scenario: Option<&str>,
    ) -> Result<Vec<SearchHit>, IndexerError> {
        self.search_with(
            q,
            k,
            page_type,
            skill,
            as_of,
            scenario,
            Granularity::Block,
            None,
        )
        .await
    }

    /// Hybrid block/page search. Returns up to `k` [`SearchHit`]s
    /// ordered by RRF-fused score descending.
    ///
    /// SQL-pushed filters narrow both the vector and FTS sides before
    /// fusion (`page_type`, `skill`, `as_of`, `scenario`). The
    /// `filter` object is a frontmatter post-filter applied after
    /// hydration (see [`crate::filter`]); `granularity` controls
    /// block- vs page-level collapse.
    #[allow(clippy::too_many_arguments)]
    pub async fn search_with(
        &self,
        q: &str,
        k: usize,
        page_type: Option<PageType>,
        skill: Option<&str>,
        as_of: Option<&str>,
        scenario: Option<&str>,
        granularity: Granularity,
        filter: Option<&serde_json::Value>,
    ) -> Result<Vec<SearchHit>, IndexerError> {
        if k == 0 {
            return Ok(Vec::new());
        }

        // 1. Embed query (outside the mutex — read-only, no
        // ordering hazard like the update_page write path had).
        let q_embeddings = self.embedder.embed(&[q]).await?;
        let q_vec = q_embeddings.into_iter().next().ok_or_else(|| {
            IndexerError::Embed(escurel_embed::EmbedError::Backend(
                "embedder returned no vectors for the query".to_owned(),
            ))
        })?;
        if q_vec.len() != BLOCKS_DENSE_VEC_DIM {
            return Err(IndexerError::EmbedderDimMismatch {
                expected: BLOCKS_DENSE_VEC_DIM,
                got: q_vec.len(),
            });
        }
        let q_lit = crate::indexer::format_vector_literal(&q_vec);

        // 2. Build filter SQL + params shared by both halves.
        let (filter_sql, filter_params) = build_filters(page_type, skill, as_of, scenario);
        let n_candidates = candidate_pool(k);

        let conn = self.conn.lock().await;

        // 3. Vector candidates.
        let vec_sql = format!(
            "SELECT block_id FROM blocks \
             WHERE 1=1{filter_sql} \
             ORDER BY array_cosine_distance(dense_vec, {q_lit}::FLOAT[{BLOCKS_DENSE_VEC_DIM}]) \
             LIMIT {n_candidates}",
        );
        let vec_ranked = run_ranking(&conn, &vec_sql, &filter_params)?;

        // 4. FTS candidates.
        let fts_sql = format!(
            "SELECT block_id FROM blocks \
             WHERE fts_main_blocks.match_bm25(block_id, ?) IS NOT NULL{filter_sql} \
             ORDER BY fts_main_blocks.match_bm25(block_id, ?) DESC \
             LIMIT {n_candidates}",
        );
        let fts_ranked = run_fts_ranking(&conn, &fts_sql, q, &filter_params)?;

        // 5. RRF fusion.
        let mut scores: HashMap<String, f64> = HashMap::new();
        accumulate_rrf(&mut scores, &vec_ranked);
        accumulate_rrf(&mut scores, &fts_ranked);
        let mut ranked: Vec<(String, f64)> = scores.into_iter().collect();
        ranked.sort_by(|a, b| {
            b.1.partial_cmp(&a.1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.0.cmp(&b.0))
        });

        // 6. Hydrate all ranked candidates in ONE bulk query, then
        // walk `ranked` to assemble hits in rank order — applying the
        // frontmatter post-filter and page collapse as we go, stopping
        // once we have `k` hits. Filtering before the cut (rather than
        // after a `truncate(k)`) keeps a filtered search returning up
        // to `k`. The bulk fetch replaces a per-candidate query_row.
        let block_ids: Vec<&str> = ranked.iter().map(|(id, _)| id.as_str()).collect();
        let hydrated = hydrate_blocks(&conn, &block_ids)?;
        let mut hits = Vec::with_capacity(k);
        let mut seen_pages: HashSet<String> = HashSet::new();
        for (block_id, score) in ranked {
            if hits.len() >= k {
                break;
            }
            let Some(parts) = hydrated.get(&block_id) else {
                continue;
            };
            let hit = parts.clone().into_hit(score);
            if let Some(f) = filter
                && !crate::filter::matches_filter(f, &hit.frontmatter_excerpt)
            {
                continue;
            }
            match granularity {
                Granularity::Block => hits.push(hit),
                // One hit per page: the first (best-scoring) block wins;
                // the block anchor is dropped for a page-level hit.
                Granularity::Page => {
                    if seen_pages.insert(hit.page_id.clone()) {
                        hits.push(SearchHit {
                            anchor: None,
                            ..hit
                        });
                    }
                }
            }
        }
        Ok(hits)
    }
}

/// Build the shared `WHERE` tail (page-type, skill, `as_of`, and
/// `scenario`) plus the bind params in `?` order. `as_of` keeps blocks
/// born at or before the cut (`blocks.at_ts <= ?`); untimed blocks
/// (`at_ts IS NULL`) stay visible so skills and non-event pages never
/// drop out of search. `scenario` keeps base blocks (and the overlay's
/// when set); base-only when `None`.
fn build_filters(
    page_type: Option<PageType>,
    skill: Option<&str>,
    as_of: Option<&str>,
    scenario: Option<&str>,
) -> (String, Vec<String>) {
    let mut sql = String::new();
    let mut params = Vec::new();
    if let Some(pt) = page_type {
        sql.push_str(" AND blocks.page_type = ?");
        params.push(
            match pt {
                PageType::Skill => "skill",
                PageType::Instance => "instance",
            }
            .to_owned(),
        );
    }
    if let Some(s) = skill {
        sql.push_str(" AND blocks.skill = ?");
        params.push(s.to_owned());
    }
    if let Some(ts) = as_of {
        sql.push_str(" AND (blocks.at_ts <= ? OR blocks.at_ts IS NULL)");
        params.push(ts.to_owned());
    }
    match scenario {
        Some(sc) => {
            sql.push_str(" AND (blocks.scenario = ? OR blocks.scenario IS NULL)");
            params.push(sc.to_owned());
        }
        None => sql.push_str(" AND blocks.scenario IS NULL"),
    }
    (sql, params)
}

fn run_ranking(
    conn: &duckdb::Connection,
    sql: &str,
    filter_params: &[String],
) -> Result<Vec<String>, IndexerError> {
    let mut stmt = conn.prepare(sql)?;
    Ok(collect(stmt.query_map(
        duckdb::params_from_iter(filter_params.iter()),
        |r| r.get(0),
    )?))
}

fn run_fts_ranking(
    conn: &duckdb::Connection,
    sql: &str,
    q: &str,
    filter_params: &[String],
) -> Result<Vec<String>, IndexerError> {
    // Param order mirrors the SQL: the match_bm25 query term, then the
    // shared filter binds, then the match_bm25 term again for ORDER BY.
    let mut binds: Vec<String> = Vec::with_capacity(filter_params.len() + 2);
    binds.push(q.to_owned());
    binds.extend_from_slice(filter_params);
    binds.push(q.to_owned());
    let mut stmt = conn.prepare(sql)?;
    Ok(collect(stmt.query_map(
        duckdb::params_from_iter(binds.iter()),
        |r| r.get(0),
    )?))
}

fn collect<I, T>(rows: I) -> Vec<T>
where
    I: Iterator<Item = duckdb::Result<T>>,
{
    rows.filter_map(std::result::Result::ok).collect()
}

fn accumulate_rrf(scores: &mut HashMap<String, f64>, ranked: &[String]) {
    for (rank, block_id) in ranked.iter().enumerate() {
        let contrib = 1.0 / (K_RRF + rank as f64 + 1.0);
        *scores.entry(block_id.clone()).or_insert(0.0) += contrib;
    }
}

/// The score-independent fields of a search hit, hydrated once per
/// candidate block. [`into_hit`](Self::into_hit) stamps the RRF score
/// on at assembly time.
#[derive(Clone)]
struct HydratedBlock {
    page_id: String,
    slug: Option<String>,
    skill: String,
    page_type: PageType,
    anchor: Option<String>,
    snippet: String,
    frontmatter_excerpt: serde_json::Value,
}

impl HydratedBlock {
    fn into_hit(self, score: f64) -> SearchHit {
        SearchHit {
            page_id: self.page_id,
            slug: self.slug,
            skill: self.skill,
            page_type: self.page_type,
            anchor: self.anchor,
            snippet: self.snippet,
            score,
            frontmatter_excerpt: self.frontmatter_excerpt,
        }
    }
}

/// Bulk-hydrate every candidate block in a single `IN (…)` query,
/// keyed by `block_id`. Replaces the per-candidate `query_row`; the
/// caller walks the ranked list and assembles hits in rank order.
/// Replicates the original column set + NULL handling exactly (empty
/// `anchor` is normalised to `None`).
fn hydrate_blocks(
    conn: &duckdb::Connection,
    block_ids: &[&str],
) -> Result<HashMap<String, HydratedBlock>, IndexerError> {
    let mut out = HashMap::with_capacity(block_ids.len());
    if block_ids.is_empty() {
        return Ok(out);
    }
    let placeholders = std::iter::repeat_n("?", block_ids.len())
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!(
        "SELECT b.block_id, b.page_id, b.anchor, b.body, p.slug, p.skill, p.page_type, \
                p.frontmatter::VARCHAR \
         FROM blocks b JOIN pages p USING (page_id) WHERE b.block_id IN ({placeholders})"
    );
    let mut stmt = conn.prepare(&sql)?;
    let mut rows = stmt.query(duckdb::params_from_iter(block_ids.iter()))?;
    while let Some(r) = rows.next()? {
        let block_id: String = r.get(0)?;
        let page_id: String = r.get(1)?;
        let anchor: Option<String> = r.get(2)?;
        let body: String = r.get(3)?;
        let slug: Option<String> = r.get(4)?;
        let skill: String = r.get(5)?;
        let page_type_str: String = r.get(6)?;
        let fm_json: String = r.get(7)?;
        let page_type = match page_type_str.as_str() {
            "skill" => PageType::Skill,
            _ => PageType::Instance,
        };
        let frontmatter_excerpt: serde_json::Value = serde_json::from_str(&fm_json)?;
        out.insert(
            block_id,
            HydratedBlock {
                page_id,
                slug,
                skill,
                page_type,
                anchor: anchor.filter(|a| !a.is_empty()),
                snippet: snippet_from_body(&body),
                frontmatter_excerpt,
            },
        );
    }
    Ok(out)
}

fn snippet_from_body(body: &str) -> String {
    // Cheap snippet: first ~200 chars, trimmed at a word boundary
    // when possible. Real query-aware snippet generation lands
    // later — agents currently get enough context from the full
    // body via `expand` if they want more.
    const MAX_CHARS: usize = 200;
    let char_indices: Vec<_> = body.char_indices().collect();
    if char_indices.len() <= MAX_CHARS {
        return body.trim().to_owned();
    }
    let limit_byte_idx = char_indices[MAX_CHARS].0;
    let sliced = &body[..limit_byte_idx];
    let cut = sliced.rfind(char::is_whitespace).unwrap_or(limit_byte_idx);
    let mut s = body[..cut].trim().to_owned();
    s.push('…');
    s
}

#[cfg(test)]
mod tests {
    use super::snippet_from_body;

    #[test]
    fn snippet_does_not_panic_on_multibyte_truncation() {
        // A >200-char body of 3-byte chars: the old `body[..200]` sliced mid
        // UTF-8 char and panicked. The fix truncates on a char boundary.
        let body = "☃".repeat(201); // 201 chars, 603 bytes
        let s = snippet_from_body(&body);
        assert!(s.ends_with('…'));
        assert!(s.starts_with('☃'));
    }

    #[test]
    fn snippet_short_body_returned_verbatim() {
        assert_eq!(snippet_from_body("  hi there  "), "hi there");
    }
}
