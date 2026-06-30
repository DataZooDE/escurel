//! Integration tests for the post-fusion rerank stage
//! (`Indexer::rerank_hits`).
//!
//! Real DuckDB + real FsStore + `HashEmbedder` for the first-stage
//! retrieval; a deterministic in-test `Reranker` exercises the stage
//! itself. The cross-encoder model lives behind `escurel-embed`'s
//! `rerank` feature and is tested there; here we only prove the
//! wiring: rerank **reorders** the candidate list and **never adds or
//! drops** a hit (so it cannot violate INV-ACL-FUSION — it can only
//! re-order rows the caller was already entitled to see).

use std::collections::BTreeSet;
use std::sync::Arc;

use async_trait::async_trait;
use bytes::Bytes;
use duckdb::Connection;
use escurel_embed::{Candidate, EmbedError, Embedder, HashEmbedder, Ranked, Reranker};
use escurel_index::{Indexer, Migrator, RetrievalConfig};
use escurel_storage::{FsStore, Key, LaneStore};
use tempfile::TempDir;

const TENANT: &str = "acme";

/// A deterministic reranker that floats candidates whose passage text
/// contains `keyword` to the top (score 1.0), everything else 0.0.
/// Stable on ties (input order preserved within a score band).
#[derive(Debug)]
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
        // Descending score, stable on the original index for ties.
        ranked.sort_by(|a, b| {
            b.1.score
                .partial_cmp(&a.1.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.0.cmp(&b.0))
        });
        Ok(ranked.into_iter().map(|(_, r)| r).collect())
    }
}

struct Harness {
    store: Arc<dyn LaneStore>,
    indexer: Indexer,
    _store_dir: TempDir,
    _db_dir: TempDir,
}

/// Build a harness whose indexer has the rerank stage wired to
/// `reranker`. Pass `None` for the default (rerank disabled) indexer.
fn harness_with(reranker: Option<Arc<dyn Reranker>>) -> Harness {
    let store_dir = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    let store: Arc<dyn LaneStore> = Arc::new(FsStore::new(store_dir.path().to_path_buf()));
    let embedder: Arc<dyn Embedder> = Arc::new(HashEmbedder::default());
    let conn = Connection::open(db_dir.path().join("escurel.duckdb")).unwrap();
    Migrator::up(&conn).unwrap();
    let mut indexer = Indexer::new(Arc::clone(&store), embedder, conn, TENANT).unwrap();
    if let Some(r) = reranker {
        indexer = indexer.with_reranker(r, RetrievalConfig::enabled(100));
    }
    Harness {
        store,
        indexer,
        _store_dir: store_dir,
        _db_dir: db_dir,
    }
}

async fn seed(h: &Harness, pages: &[(&str, &'static str)]) {
    for (path, body) in pages {
        let key = Key::new(TENANT, path.to_owned()).unwrap();
        h.store
            .write(&key, Bytes::from_static(body.as_bytes()))
            .await
            .unwrap();
        h.indexer.update_page(path, body).await.unwrap();
    }
    h.indexer.refresh_fts().await.unwrap();
}

const SKILL_NOTE: (&str, &str) = (
    "markdown/skills/note.md",
    "---\ntype: skill\nid: note\ndescription: A note.\n---\n# note\n",
);

const ALPHA: (&str, &str) = (
    "markdown/instances/note/alpha.md",
    "---\ntype: instance\nskill: note\nid: alpha\n---\n# Alpha\n\nGeneral remarks about quarterly planning and logistics.\n",
);

const BETA: (&str, &str) = (
    "markdown/instances/note/beta.md",
    "---\ntype: instance\nskill: note\nid: beta\n---\n# Beta\n\nThe zebra crossing budget was approved for planning.\n",
);

const GAMMA: (&str, &str) = (
    "markdown/instances/note/gamma.md",
    "---\ntype: instance\nskill: note\nid: gamma\n---\n# Gamma\n\nMore planning notes, unrelated to anything striped.\n",
);

#[tokio::test]
async fn rerank_reorders_hits_by_score_and_preserves_the_set() {
    let h = harness_with(Some(Arc::new(KeywordReranker {
        keyword: "zebra".to_owned(),
    })));
    seed(&h, &[SKILL_NOTE, ALPHA, BETA, GAMMA]).await;

    // First-stage retrieval over a generic term; "beta" (the zebra note)
    // is just one of several "planning" hits, not necessarily first.
    let hits = h
        .indexer
        .search("planning", 10, None, Some("note"), None, None)
        .await
        .unwrap();
    assert!(hits.len() >= 2, "need several candidates to reorder");
    let before: BTreeSet<String> = hits.iter().map(|x| x.page_id.clone()).collect();

    let reranked = h.indexer.rerank_hits("planning", hits).await.unwrap();

    // The set is identical — rerank only reorders, never adds/drops.
    let after: BTreeSet<String> = reranked.iter().map(|x| x.page_id.clone()).collect();
    assert_eq!(
        before, after,
        "rerank must preserve the exact hit set (INV-ACL-FUSION safe)"
    );
    // The zebra note is now ranked first.
    assert!(
        reranked[0].page_id.ends_with("beta.md"),
        "expected the 'zebra' note promoted to the top; got {:?}",
        reranked.iter().map(|h| &h.page_id).collect::<Vec<_>>(),
    );
    // Scores descend after rerank.
    assert!(reranked.windows(2).all(|w| w[0].score >= w[1].score));
}

// A note whose distinguishing token `zebracorn` sits AFTER the first ~200
// chars, so it is absent from the hydrated 200-char snippet and present only in
// the full block body. The lead matches "planning" so FTS retrieves it.
const TAIL: (&str, &str) = (
    "markdown/instances/note/tail.md",
    "---\ntype: instance\nskill: note\nid: tail\n---\n# Tail\n\n\
     Quarterly planning notes covering logistics, budgets, staffing, vendors, \
     timelines, milestones, dependencies, risks, owners, reviewers, approvers, \
     and stakeholders across every regional division and operating unit in the \
     company. zebracorn appears only here, well past the snippet lead.\n",
);

#[tokio::test]
async fn rerank_scores_full_body_not_just_the_snippet() {
    // The reranker promotes whatever passage contains `zebracorn`. That token
    // lives only in TAIL's body tail (beyond the 200-char snippet), so TAIL can
    // be promoted ONLY if rerank feeds the cross-encoder the full body.
    let h = harness_with(Some(Arc::new(KeywordReranker {
        keyword: "zebracorn".to_owned(),
    })));
    seed(&h, &[SKILL_NOTE, ALPHA, BETA, GAMMA, TAIL]).await;

    let hits = h
        .indexer
        .search("planning", 10, None, Some("note"), None, None)
        .await
        .unwrap();
    assert!(
        hits.iter().any(|x| x.page_id.ends_with("tail.md")),
        "FTS must retrieve the tail note via its 'planning' lead",
    );
    // The snippet alone must NOT contain the tail token (guards the premise).
    let tail_hit = hits
        .iter()
        .find(|x| x.page_id.ends_with("tail.md"))
        .unwrap();
    assert!(
        !tail_hit.snippet.contains("zebracorn"),
        "premise: the token must be outside the snippet; got {:?}",
        tail_hit.snippet,
    );

    let reranked = h.indexer.rerank_hits("planning", hits).await.unwrap();
    assert!(
        reranked[0].page_id.ends_with("tail.md"),
        "full-body rerank must promote the tail-token note; got {:?}",
        reranked.iter().map(|h| &h.page_id).collect::<Vec<_>>(),
    );
}

#[tokio::test]
async fn rerank_disabled_by_default_is_identity() {
    let h = harness_with(None);
    seed(&h, &[SKILL_NOTE, ALPHA, BETA, GAMMA]).await;

    let hits = h
        .indexer
        .search("planning", 10, None, Some("note"), None, None)
        .await
        .unwrap();
    assert!(!h.indexer.rerank_enabled());

    let same = h
        .indexer
        .rerank_hits("planning", hits.clone())
        .await
        .unwrap();
    assert_eq!(hits, same, "with rerank disabled the list is untouched");
}
