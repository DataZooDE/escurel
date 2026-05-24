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
//! ## What does NOT ship in this PR
//!
//! - Page-granularity hits (collapsing adjacent block hits).
//! - Frontmatter filter clauses (`{at: {">=": "..."}}`).
//! - The ADR-0001 retrieval-quality gate. That gate requires a
//!   real embedder; M2.2 (EmbeddingGemma) lands the embedder,
//!   then the gate is the first thing to run before production.

use std::collections::HashMap;

use duckdb::params;
use escurel_md::PageType;

use crate::indexer::BLOCKS_DENSE_VEC_DIM;
use crate::{Indexer, IndexerError};

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

    /// Hybrid block search. Returns up to `k` [`SearchHit`]s
    /// ordered by RRF-fused score descending.
    ///
    /// Optional filters narrow both the vector and FTS sides
    /// before fusion:
    /// - `page_type` — restrict to skill or instance blocks.
    /// - `skill` — restrict to blocks belonging to pages of one
    ///   skill.
    pub async fn search(
        &self,
        q: &str,
        k: usize,
        page_type: Option<PageType>,
        skill: Option<&str>,
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
        let (filter_sql, filter_pt, filter_skill) = build_filters(page_type, skill);
        let n_candidates = candidate_pool(k);

        let conn = self.conn.lock().await;

        // 3. Vector candidates.
        let vec_sql = format!(
            "SELECT block_id FROM blocks \
             WHERE 1=1{filter_sql} \
             ORDER BY array_cosine_distance(dense_vec, {q_lit}::FLOAT[{BLOCKS_DENSE_VEC_DIM}]) \
             LIMIT {n_candidates}",
        );
        let vec_ranked = run_ranking(&conn, &vec_sql, &filter_pt, &filter_skill)?;

        // 4. FTS candidates.
        let fts_sql = format!(
            "SELECT block_id FROM blocks \
             WHERE fts_main_blocks.match_bm25(block_id, ?) IS NOT NULL{filter_sql} \
             ORDER BY fts_main_blocks.match_bm25(block_id, ?) DESC \
             LIMIT {n_candidates}",
        );
        let fts_ranked = run_fts_ranking(&conn, &fts_sql, q, &filter_pt, &filter_skill)?;

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
        ranked.truncate(k);

        // 6. Hydrate hits.
        let mut hits = Vec::with_capacity(ranked.len());
        for (block_id, score) in ranked {
            if let Some(hit) = hydrate_hit(&conn, &block_id, score)? {
                hits.push(hit);
            }
        }
        Ok(hits)
    }
}

fn build_filters(
    page_type: Option<PageType>,
    skill: Option<&str>,
) -> (String, Option<&'static str>, Option<String>) {
    let mut sql = String::new();
    let pt_param = page_type.map(|pt| match pt {
        PageType::Skill => "skill",
        PageType::Instance => "instance",
    });
    let skill_param = skill.map(str::to_owned);
    if pt_param.is_some() {
        sql.push_str(" AND blocks.page_type = ?");
    }
    if skill_param.is_some() {
        sql.push_str(" AND blocks.skill = ?");
    }
    (sql, pt_param, skill_param)
}

fn run_ranking(
    conn: &duckdb::Connection,
    sql: &str,
    pt: &Option<&'static str>,
    skill: &Option<String>,
) -> Result<Vec<String>, IndexerError> {
    let mut stmt = conn.prepare(sql)?;
    let block_ids: Vec<String> = match (pt, skill) {
        (Some(p), Some(s)) => collect(stmt.query_map(params![p, s], |r| r.get(0))?),
        (Some(p), None) => collect(stmt.query_map(params![p], |r| r.get(0))?),
        (None, Some(s)) => collect(stmt.query_map(params![s], |r| r.get(0))?),
        (None, None) => collect(stmt.query_map([], |r| r.get(0))?),
    };
    Ok(block_ids)
}

fn run_fts_ranking(
    conn: &duckdb::Connection,
    sql: &str,
    q: &str,
    pt: &Option<&'static str>,
    skill: &Option<String>,
) -> Result<Vec<String>, IndexerError> {
    let mut stmt = conn.prepare(sql)?;
    let block_ids: Vec<String> = match (pt, skill) {
        (Some(p), Some(s)) => collect(stmt.query_map(params![q, p, s, q], |r| r.get(0))?),
        (Some(p), None) => collect(stmt.query_map(params![q, p, q], |r| r.get(0))?),
        (None, Some(s)) => collect(stmt.query_map(params![q, s, q], |r| r.get(0))?),
        (None, None) => collect(stmt.query_map(params![q, q], |r| r.get(0))?),
    };
    Ok(block_ids)
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

fn hydrate_hit(
    conn: &duckdb::Connection,
    block_id: &str,
    score: f64,
) -> Result<Option<SearchHit>, IndexerError> {
    let row = conn
        .query_row(
            "SELECT b.page_id, b.anchor, b.body, p.slug, p.skill, p.page_type, \
                    p.frontmatter::VARCHAR \
             FROM blocks b JOIN pages p USING (page_id) WHERE b.block_id = ?",
            params![block_id],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, Option<String>>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, Option<String>>(3)?,
                    r.get::<_, String>(4)?,
                    r.get::<_, String>(5)?,
                    r.get::<_, String>(6)?,
                ))
            },
        )
        .ok();
    let Some((page_id, anchor, body, slug, skill, page_type_str, fm_json)) = row else {
        return Ok(None);
    };
    let page_type = match page_type_str.as_str() {
        "skill" => PageType::Skill,
        _ => PageType::Instance,
    };
    let frontmatter_excerpt: serde_json::Value = serde_json::from_str(&fm_json)?;
    let snippet = snippet_from_body(&body);
    Ok(Some(SearchHit {
        page_id,
        slug,
        skill,
        page_type,
        anchor: anchor.filter(|a| !a.is_empty()),
        snippet,
        score,
        frontmatter_excerpt,
    }))
}

fn snippet_from_body(body: &str) -> String {
    // Cheap snippet: first ~200 chars, trimmed at a word boundary
    // when possible. Real query-aware snippet generation lands
    // later — agents currently get enough context from the full
    // body via `expand` if they want more.
    const MAX: usize = 200;
    if body.len() <= MAX {
        return body.trim().to_owned();
    }
    let cut = body[..MAX].rfind(char::is_whitespace).unwrap_or(MAX);
    let mut s = body[..cut].trim().to_owned();
    s.push('…');
    s
}
