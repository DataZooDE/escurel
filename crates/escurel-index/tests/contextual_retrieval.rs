//! Integration tests for Contextual Retrieval, Variant A (structural),
//! GH #216.
//!
//! Real DuckDB + real FsStore + real `HashEmbedder` + the document ingest
//! worker. No mocks. The contract under test is the storage matrix the
//! issue recommends ("keep `body` verbatim for display/provenance and store
//! the prefix separately, concatenating only at embed + FTS-index time"):
//!
//! | representation              | carries the structural prefix? |
//! |-----------------------------|--------------------------------|
//! | `blocks.body` (expand/snippet display) | NO — verbatim chunk  |
//! | `blocks.context` (new column)          | YES — `[title › headings › p.N]` |
//! | dense embedding input                  | YES — `context\n body` |
//! | BM25/FTS index                         | YES — indexes body + context |
//! | rerank passage                         | YES — `context\n body` |

use std::sync::Arc;

use async_trait::async_trait;
use bytes::Bytes;
use duckdb::Connection;
use escurel_embed::{Candidate, EmbedError, Embedder, HashEmbedder, Ranked, Reranker};
use escurel_index::backend::{
    Chunk, ChunkConfig, DocMetadata, DocumentIngestWorker, DocumentProcessor, ExtractConfig,
    ExtractError, ExtractionResult, IngestOutcome, OcrPolicy,
};
use escurel_index::{Indexer, Migrator, RetrievalConfig};
use escurel_storage::{FsStore, LaneStore};
use tempfile::TempDir;

const TENANT: &str = "acme";

// The title token `Zephyrology` does NOT appear in the chunk body below; only
// the structural prefix can put it into the indexed representation.
const TITLE_TOKEN: &str = "Zephyrology";
const DOC_TITLE: &str = "Zephyrology Field Manual";
const CHUNK_BODY: &str = "Wind speeds were measured across three coastal stations during spring.";

const MEMO_SKILL: &str = "\
---
type: skill
id: memo
description: text memos
backend:
  kind: document
  accepts: [text/plain]
---
# memo
";

/// A processor that yields a known title + a single paged chunk, so the test
/// controls exactly what the structural prefix should be built from. (The
/// real `PlainTextExtractor` carries no title; we need one here.)
struct TitledProcessor;

#[async_trait]
impl DocumentProcessor for TitledProcessor {
    fn engine(&self) -> String {
        "titled-test@1".to_owned()
    }
    async fn process(
        &self,
        bytes: &[u8],
        _mime: &str,
        _cfg: &ExtractConfig,
    ) -> Result<ExtractionResult, ExtractError> {
        let content = std::str::from_utf8(bytes).unwrap().to_owned();
        Ok(ExtractionResult {
            metadata: DocMetadata {
                title: Some(DOC_TITLE.to_owned()),
                ..Default::default()
            },
            chunks: vec![Chunk {
                ordinal: 0,
                byte_start: 0,
                byte_end: content.len(),
                page: Some(7),
                text: content.clone(),
            }],
            content,
        })
    }
}

fn indexer_on(store: Arc<dyn LaneStore>, db: &TempDir) -> Arc<Indexer> {
    let conn = Connection::open(db.path().join("escurel.duckdb")).unwrap();
    Migrator::up(&conn).unwrap();
    let embedder: Arc<dyn Embedder> = Arc::new(HashEmbedder::default());
    Arc::new(Indexer::new(store, embedder, conn, TENANT).unwrap())
}

fn cfg() -> ExtractConfig {
    ExtractConfig {
        ocr: OcrPolicy::Off,
        chunk: ChunkConfig::default(),
    }
}

async fn ingest_with(
    indexer: &Arc<Indexer>,
    store: &Arc<dyn LaneStore>,
    processor: Arc<dyn DocumentProcessor>,
    instance_id: &str,
    body: &str,
) -> String {
    indexer
        .update_page("markdown/skills/memo.md", MEMO_SKILL)
        .await
        .unwrap();
    let blob = store
        .put_inbox_blob(TENANT, Bytes::from(body.as_bytes().to_vec()), None)
        .await
        .unwrap();
    let worker = DocumentIngestWorker::new(Arc::clone(indexer), processor);
    let out = worker
        .ingest(
            &blob,
            "text/plain",
            "memo",
            instance_id,
            &cfg(),
            &serde_json::Value::Null,
        )
        .await
        .unwrap();
    match out {
        IngestOutcome::Materialised { page_id, .. } => page_id,
        other => panic!("expected materialised, got {other:?}"),
    }
}

/// The FTS lane sees the structural context (a query for the title token —
/// absent from every chunk body — retrieves the chunk), while the DISPLAYED
/// representation stays clean: `expand` returns the verbatim chunk text with
/// no `[…]` prefix (the issue's recommended split: prefix at embed/index
/// time only, `blocks.body` verbatim for display + provenance).
#[tokio::test]
async fn title_token_retrieves_chunk_but_display_body_stays_clean() {
    let store_dir = TempDir::new().unwrap();
    let store: Arc<dyn LaneStore> = Arc::new(FsStore::new(store_dir.path().to_path_buf()));
    let db = TempDir::new().unwrap();
    let i = indexer_on(Arc::clone(&store), &db);

    let page_id = ingest_with(&i, &store, Arc::new(TitledProcessor), "doc-z", CHUNK_BODY).await;
    i.refresh_fts().await.unwrap();

    // BM25 lane: the title token is absent from the raw chunk but present in
    // the structural context → it must retrieve the chunk.
    let hits = i
        .search(TITLE_TOKEN, 10, None, None, None, None)
        .await
        .unwrap();
    let hit = hits
        .iter()
        .find(|h| h.page_id == page_id)
        .unwrap_or_else(|| {
            panic!(
                "title token `{TITLE_TOKEN}` must retrieve the contextualised chunk; hits: {hits:?}"
            )
        });

    // The snippet shown to callers is derived from the verbatim body — clean.
    assert!(
        hit.snippet.starts_with("Wind speeds"),
        "snippet must come from the verbatim chunk body, got: {:?}",
        hit.snippet
    );

    // expand: the displayed block content is the VERBATIM chunk — no prefix.
    let page = i.expand(&page_id, None, None).await.unwrap().expect("page");
    let first = &page.blocks.first().expect("a chunk block").content;
    assert_eq!(
        first, CHUNK_BODY,
        "expand must return the verbatim chunk body (prefix lives in the \
         context column, not the display text)"
    );
}

/// The exact storage matrix: `blocks.body` verbatim, `blocks.context` carries
/// the `[<title> › p.<page>]` prefix. Inspected by reopening the DuckDB file
/// after the indexer is dropped.
#[tokio::test]
async fn stored_matrix_body_verbatim_context_carries_prefix() {
    let store_dir = TempDir::new().unwrap();
    let store: Arc<dyn LaneStore> = Arc::new(FsStore::new(store_dir.path().to_path_buf()));
    let db = TempDir::new().unwrap();
    let i = indexer_on(Arc::clone(&store), &db);

    let _ = ingest_with(&i, &store, Arc::new(TitledProcessor), "doc-z", CHUNK_BODY).await;
    drop(i);

    let conn = Connection::open(db.path().join("escurel.duckdb")).unwrap();
    Migrator::load_extensions(&conn).unwrap();
    let (body, context): (String, Option<String>) = conn
        .query_row(
            "SELECT body, context FROM blocks WHERE page_type = 'instance'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(body, CHUNK_BODY, "blocks.body must stay verbatim");
    assert_eq!(
        context.as_deref(),
        Some("[Zephyrology Field Manual \u{203a} p.7]"),
        "blocks.context must carry the structural prefix"
    );
}

/// `ContextualizeMode::Off` restores the legacy representation: no context
/// column value, verbatim body (the operator's cutover switch,
/// `ESCUREL_INGEST_CONTEXTUALIZE=off`).
#[tokio::test]
async fn contextualize_off_stores_no_context() {
    use escurel_index::backend::ContextualizeMode;

    let store_dir = TempDir::new().unwrap();
    let store: Arc<dyn LaneStore> = Arc::new(FsStore::new(store_dir.path().to_path_buf()));
    let db = TempDir::new().unwrap();
    let i = indexer_on(Arc::clone(&store), &db);

    i.update_page("markdown/skills/memo.md", MEMO_SKILL)
        .await
        .unwrap();
    let blob = store
        .put_inbox_blob(TENANT, Bytes::from_static(CHUNK_BODY.as_bytes()), None)
        .await
        .unwrap();
    let worker = DocumentIngestWorker::new(Arc::clone(&i), Arc::new(TitledProcessor))
        .with_contextualize(ContextualizeMode::Off);
    worker
        .ingest(
            &blob,
            "text/plain",
            "memo",
            "doc-off",
            &cfg(),
            &serde_json::Value::Null,
        )
        .await
        .unwrap();
    drop(worker);
    drop(i);

    let conn = Connection::open(db.path().join("escurel.duckdb")).unwrap();
    Migrator::load_extensions(&conn).unwrap();
    let (body, context): (String, Option<String>) = conn
        .query_row(
            "SELECT body, context FROM blocks WHERE page_type = 'instance'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(body, CHUNK_BODY);
    assert_eq!(context, None, "Off mode must store no context");
}

// ---------------------------------------------------------------------------
// Heading path: the section hierarchy above a chunk situates it.
// ---------------------------------------------------------------------------

const LOG_TITLE: &str = "Expedition Log";
// Two similarly-worded chunks; only the HEADINGS distinguish them. The query
// token `Maritime` appears in a heading, never in a chunk body.
const SECTION_A: &str = "Team performance improved measurably during the second phase.";
const SECTION_B: &str = "Team performance improved measurably during the third phase.";

/// A processor emitting one chunk per `##` section of a markdown document,
/// with byte spans into `content` — the shape a heading-aware splitter
/// produces, letting the worker derive each chunk's heading path.
struct SectionedProcessor;

#[async_trait]
impl DocumentProcessor for SectionedProcessor {
    fn engine(&self) -> String {
        "sectioned-test@1".to_owned()
    }
    async fn process(
        &self,
        bytes: &[u8],
        _mime: &str,
        _cfg: &ExtractConfig,
    ) -> Result<ExtractionResult, ExtractError> {
        let content = std::str::from_utf8(bytes).unwrap().to_owned();
        let chunks = [SECTION_A, SECTION_B]
            .iter()
            .enumerate()
            .map(|(i, text)| {
                let byte_start = content.find(text).expect("section body in content");
                Chunk {
                    ordinal: i as u32,
                    byte_start,
                    byte_end: byte_start + text.len(),
                    page: None,
                    text: (*text).to_owned(),
                }
            })
            .collect();
        Ok(ExtractionResult {
            metadata: DocMetadata {
                title: Some(LOG_TITLE.to_owned()),
                ..Default::default()
            },
            chunks,
            content,
        })
    }
}

fn sectioned_content() -> String {
    format!("## Alpine Expedition\n{SECTION_A}\n\n## Maritime Expedition\n{SECTION_B}\n")
}

/// The end-to-end point of #216: two similarly-worded chunks under different
/// headings; a query naming the heading context retrieves the RIGHT one. The
/// token `Maritime` exists only in the second section's heading, so only the
/// heading path in that chunk's structural context can match it.
#[tokio::test]
async fn heading_context_disambiguates_similar_chunks() {
    let store_dir = TempDir::new().unwrap();
    let store: Arc<dyn LaneStore> = Arc::new(FsStore::new(store_dir.path().to_path_buf()));
    let db = TempDir::new().unwrap();
    let i = indexer_on(Arc::clone(&store), &db);

    let page_id = ingest_with(
        &i,
        &store,
        Arc::new(SectionedProcessor),
        "doc-exp",
        &sectioned_content(),
    )
    .await;
    i.refresh_fts().await.unwrap();

    let hits = i
        .search("Maritime", 10, None, None, None, None)
        .await
        .unwrap();
    let first = hits
        .iter()
        .find(|h| h.page_id == page_id)
        .unwrap_or_else(|| panic!("heading token must retrieve the document; hits: {hits:?}"));
    assert_eq!(
        first.anchor.as_deref(),
        Some("chunk-1"),
        "the chunk under `## Maritime Expedition` must rank first for a \
         query naming its heading; hits: {hits:?}"
    );
    // And its displayed snippet is still the clean section body.
    assert!(
        first.snippet.starts_with("Team performance"),
        "snippet stays clean, got: {:?}",
        first.snippet
    );
}

// ---------------------------------------------------------------------------
// Rerank interplay: the cross-encoder scores the CONTEXTUALISED passage
// (context + body), consistent with what the dense/FTS lanes indexed.
// ---------------------------------------------------------------------------

/// Deterministic reranker: floats candidates whose passage contains
/// `keyword` (score 1.0), everything else 0.0; stable on ties.
struct KeywordReranker {
    keyword: String,
}

#[async_trait]
impl Reranker for KeywordReranker {
    async fn rerank(
        &self,
        _query: &str,
        candidates: &[Candidate],
    ) -> Result<Vec<Ranked>, EmbedError> {
        let kw = self.keyword.to_lowercase();
        let mut ranked: Vec<(usize, Ranked)> = candidates
            .iter()
            .enumerate()
            .map(|(i, c)| {
                let score = if c.text.to_lowercase().contains(&kw) {
                    1.0
                } else {
                    0.0
                };
                (
                    i,
                    Ranked {
                        id: c.id.clone(),
                        score,
                    },
                )
            })
            .collect();
        ranked.sort_by(|a, b| {
            b.1.score
                .partial_cmp(&a.1.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.0.cmp(&b.0))
        });
        Ok(ranked.into_iter().map(|(_, r)| r).collect())
    }
}

/// The rerank passage is `context + body`, not the bare body: a reranker
/// keyed on a token that exists ONLY in the structural context (the heading
/// `Maritime`) must float that chunk to the top of the whole result set.
#[tokio::test]
async fn rerank_passage_carries_structural_context() {
    let store_dir = TempDir::new().unwrap();
    let store: Arc<dyn LaneStore> = Arc::new(FsStore::new(store_dir.path().to_path_buf()));
    let db = TempDir::new().unwrap();

    let conn = Connection::open(db.path().join("escurel.duckdb")).unwrap();
    Migrator::up(&conn).unwrap();
    let embedder: Arc<dyn Embedder> = Arc::new(HashEmbedder::default());
    let i = Arc::new(
        Indexer::new(Arc::clone(&store), embedder, conn, TENANT)
            .unwrap()
            .with_reranker(
                Arc::new(KeywordReranker {
                    keyword: "maritime".to_owned(),
                }),
                RetrievalConfig::enabled(10),
            ),
    );

    let page_id = ingest_with(
        &i,
        &store,
        Arc::new(SectionedProcessor),
        "doc-exp",
        &sectioned_content(),
    )
    .await;
    i.refresh_fts().await.unwrap();

    // Query matches both chunk bodies equally; only the reranker (via the
    // contextualised passage) can prefer the Maritime section.
    let hits = i
        .search("team performance improved", 10, None, None, None, None)
        .await
        .unwrap();
    let hits = i
        .rerank_hits("team performance improved", hits)
        .await
        .unwrap();
    let first = hits
        .iter()
        .find(|h| h.page_id == page_id)
        .expect("document retrieved");
    assert_eq!(
        first.anchor.as_deref(),
        Some("chunk-1"),
        "reranker keyed on a context-only token must float the Maritime \
         chunk — the rerank passage must include the structural context; \
         hits: {hits:?}"
    );
}
