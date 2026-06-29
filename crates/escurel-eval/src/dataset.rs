//! BEIR-format dataset loader.
//!
//! A dataset directory holds three files (the convention BEIR's SciFact /
//! NFCorpus / etc. ship, and the one escurel's own future 460-block corpus
//! should adopt):
//!
//! - `corpus.jsonl`  — one JSON object per line: `{"_id", "title", "text", …}`
//! - `queries.jsonl` — one per line: `{"_id", "text", …}`
//! - `qrels/test.tsv` — TSV with a header `query-id\tcorpus-id\tscore`, then
//!   one judgment per row.
//!
//! The corpus `_id` is used **verbatim** as the escurel `page_id`, so the qrels
//! compare directly against [`escurel_index::SearchHit::page_id`] with no
//! translation table.

use std::collections::HashMap;
use std::path::Path;

use serde::Deserialize;

use crate::error::EvalError;
use crate::metrics::RelMap;

/// One corpus document. Extra JSON fields (e.g. `metadata`) are ignored.
#[derive(Debug, Clone, Deserialize)]
pub struct CorpusDoc {
    #[serde(rename = "_id")]
    pub id: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub text: String,
}

impl CorpusDoc {
    /// The text fed to the embedder + stored as the block body: the title and
    /// body joined (BEIR convention), trimmed.
    #[must_use]
    pub fn body(&self) -> String {
        match (self.title.trim(), self.text.trim()) {
            ("", t) => t.to_owned(),
            (h, "") => h.to_owned(),
            (h, t) => format!("{h}\n\n{t}"),
        }
    }
}

/// One evaluation query.
#[derive(Debug, Clone, Deserialize)]
pub struct Query {
    #[serde(rename = "_id")]
    pub id: String,
    #[serde(default)]
    pub text: String,
}

/// query id → ([`RelMap`]: corpus id → graded relevance).
pub type Qrels = HashMap<String, RelMap>;

/// A loaded BEIR dataset.
#[derive(Debug, Clone)]
pub struct Dataset {
    pub name: String,
    pub corpus: Vec<CorpusDoc>,
    pub queries: Vec<Query>,
    pub qrels: Qrels,
}

impl Dataset {
    /// Load `corpus.jsonl` + `queries.jsonl` + `qrels/test.tsv` from `dir`.
    /// Queries with no qrels entry are dropped (BEIR test splits judge a
    /// subset), so every retained query contributes a real metric.
    pub fn load(dir: &Path) -> Result<Self, EvalError> {
        let corpus: Vec<CorpusDoc> = read_jsonl(&dir.join("corpus.jsonl"))?;
        let all_queries: Vec<Query> = read_jsonl(&dir.join("queries.jsonl"))?;
        let qrels = read_qrels(&dir.join("qrels").join("test.tsv"))?;

        let queries = all_queries
            .into_iter()
            .filter(|q| qrels.contains_key(&q.id))
            .collect();

        let name = dir
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("dataset")
            .to_owned();
        Ok(Self {
            name,
            corpus,
            queries,
            qrels,
        })
    }

    /// The relevance map for `query_id`, or an empty map.
    #[must_use]
    pub fn rel(&self, query_id: &str) -> RelMap {
        self.qrels.get(query_id).cloned().unwrap_or_default()
    }
}

fn read_jsonl<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<Vec<T>, EvalError> {
    let raw = std::fs::read_to_string(path).map_err(|source| EvalError::Io {
        path: path.display().to_string(),
        source,
    })?;
    let mut out = Vec::new();
    for (i, line) in raw.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let item: T = serde_json::from_str(line).map_err(|e| EvalError::Dataset {
            file: path.display().to_string(),
            line: i + 1,
            reason: e.to_string(),
        })?;
        out.push(item);
    }
    Ok(out)
}

/// Parse a BEIR `qrels` TSV: a header row, then `query-id\tcorpus-id\tscore`.
/// Rows with score 0 are recorded (graded relevance), but only `score > 0`
/// counts as relevant in the metrics.
fn read_qrels(path: &Path) -> Result<Qrels, EvalError> {
    let raw = std::fs::read_to_string(path).map_err(|source| EvalError::Io {
        path: path.display().to_string(),
        source,
    })?;
    let mut qrels: Qrels = HashMap::new();
    for (i, line) in raw.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        // Skip the header line if present.
        if i == 0 && line.starts_with("query-id") {
            continue;
        }
        let mut cols = line.split('\t');
        let bad = |reason: &str| EvalError::Dataset {
            file: path.display().to_string(),
            line: i + 1,
            reason: reason.to_owned(),
        };
        let qid = cols.next().ok_or_else(|| bad("missing query-id"))?;
        let cid = cols.next().ok_or_else(|| bad("missing corpus-id"))?;
        let score: u32 = cols
            .next()
            .ok_or_else(|| bad("missing score"))?
            .trim()
            .parse()
            .map_err(|_| bad("score is not an integer"))?;
        qrels
            .entry(qid.to_owned())
            .or_default()
            .insert(cid.to_owned(), score);
    }
    Ok(qrels)
}
