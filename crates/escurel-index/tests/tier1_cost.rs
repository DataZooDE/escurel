//! The Tier-1 cost measurement behind §6.2 of `docs/paper`.
//!
//! An agent's cold start reads `list_skills`: one `(id, description)` pair per
//! skill and no bodies. The claim is that this cost does not move as the
//! corpus grows. Half of that is structural rather than empirical --- the
//! query is `WHERE page_type = 'skill'`, so the *payload* cannot depend on how
//! many instances exist, and saying otherwise would be dressing up a schema
//! fact as a discovery. What is genuinely open is the **latency**: the scan
//! runs over a `pages` table that the instances share, so Tier-1 could get
//! slower with corpus size even though it returns the same bytes.
//!
//! So this measures three things at each scale: the Tier-1 payload (confirming
//! the structural claim rather than assuming it), the Tier-1 latency, and the
//! size of the whole corpus for the counterfactual an agent avoids reading.
//!
//! **Instances are inserted directly.** Driving `update_page` 100k times would
//! measure the write path, which is not what this experiment is about; the
//! rows are written straight to `pages` so the *read* being measured sees a
//! corpus of the right shape. Nothing here should be read as a claim about
//! write throughput.
//!
//! Emits JSON to `$ESCUREL_PAPER_DATA` (default: stdout). Token counts are
//! deliberately **not** computed here: the harness reports characters, and the
//! render step converts with a named tokeniser, so the tokeniser is a property
//! of the report rather than baked into the measurement.
//!
//! `cargo test -p escurel-index --features paper-measurements --test tier1_cost -- --nocapture`
#![cfg(feature = "paper-measurements")]

use std::sync::Arc;
use std::time::Instant;

use duckdb::Connection;
use escurel_embed::{Embedder, ZeroEmbedder};
use escurel_index::{Indexer, Migrator};
use escurel_storage::{FsStore, LaneStore};
use serde_json::json;
use tempfile::TempDir;

const TENANT: &str = "acme";
/// A plausible domain vocabulary: the number of skills a team authors.
const SKILLS: usize = 24;
/// Instance counts spanning four orders of magnitude.
const SCALES: [usize; 4] = [100, 1_000, 10_000, 100_000];
const LATENCY_SAMPLES: usize = 30;

fn skill_md(i: usize) -> String {
    format!(
        "---\ntype: skill\nid: skill-{i:02}\n\
         description: Records of kind {i:02}; use this when the question is \
         about a kind-{i:02} entity, its current state, or its history.\n\
         ---\n# skill-{i:02}\n"
    )
}

fn percentile(sorted: &[f64], p: f64) -> f64 {
    let idx = ((sorted.len() as f64 - 1.0) * p).round() as usize;
    sorted[idx]
}

#[tokio::test(flavor = "multi_thread")]
async fn tier1_cost_against_corpus_size() {
    let mut rows = Vec::new();

    for &n in &SCALES {
        let store_dir = TempDir::new().unwrap();
        let db_dir = TempDir::new().unwrap();
        let store: Arc<dyn LaneStore> = Arc::new(FsStore::new(store_dir.path().to_path_buf()));
        let embedder: Arc<dyn Embedder> = Arc::new(ZeroEmbedder::default());
        let conn = Connection::open(db_dir.path().join("escurel.duckdb")).unwrap();
        Migrator::up(&conn).unwrap();

        // Instances are bulk-inserted on the same connection *before* the
        // indexer takes it, so the read being measured sees a corpus of the
        // right shape without 100k trips through the write path. `pages` holds
        // no body -- bodies live in `blocks` -- so the corpus size is computed
        // from the text that would have been written.
        let mut corpus_chars: u64 = 0;
        conn.execute_batch("BEGIN TRANSACTION").unwrap();
        {
            let mut stmt = conn
                .prepare(
                    "INSERT INTO pages (page_id, slug, skill, page_type, frontmatter, \
                     body_hash, at_ts, created_at, updated_at) \
                     VALUES (?, ?, ?, 'instance', ?, ?, NULL, now(), now())",
                )
                .expect("prepare insert");
            for k in 0..n {
                let skill = k % SKILLS;
                let id = format!("inst-{k:06}");
                let body = format!(
                    "# {id}\n\nA kind-{skill:02} entity. Current state as of the last \
                     event that touched it, with the usual surrounding detail.\n"
                );
                corpus_chars += body.len() as u64;
                stmt.execute(duckdb::params![
                    format!("markdown/instances/skill-{skill:02}/{id}.md"),
                    id,
                    format!("skill-{skill:02}"),
                    format!(
                        "{{\"type\":\"instance\",\"skill\":\"skill-{skill:02}\",\"id\":\"{id}\"}}"
                    ),
                    format!("{k:016x}"),
                ])
                .expect("insert page row");
            }
        }
        conn.execute_batch("COMMIT").unwrap();

        let indexer = Arc::new(Indexer::new(store, embedder, conn, TENANT).unwrap());

        // The skills go through the real write path: they are what Tier-1
        // returns, so their frontmatter must be genuine.
        for i in 0..SKILLS {
            indexer
                .update_page(&format!("markdown/skills/skill-{i:02}.md"), &skill_md(i))
                .await
                .unwrap();
        }

        // Tier 1: what the agent actually reads at cold start.
        let skills = indexer.list_skills().await.unwrap();
        let tier1_chars: u64 = skills
            .iter()
            .map(|s| (s.id.len() + s.description.len() + 2) as u64)
            .sum();

        let mut ms = Vec::with_capacity(LATENCY_SAMPLES);
        for _ in 0..LATENCY_SAMPLES {
            let t = Instant::now();
            let _ = indexer.list_skills().await.unwrap();
            ms.push(t.elapsed().as_secs_f64() * 1000.0);
        }
        ms.sort_by(|a, b| a.partial_cmp(b).unwrap());

        rows.push(json!({
            "instances": n,
            "skills": SKILLS,
            "tier1_chars": tier1_chars,
            "tier1_entries": skills.len(),
            "tier1_p50_ms": (percentile(&ms, 0.50) * 1000.0).round() / 1000.0,
            "tier1_p95_ms": (percentile(&ms, 0.95) * 1000.0).round() / 1000.0,
            "whole_corpus_chars": corpus_chars,
        }));
    }

    let out = json!({
        "note": "tier1 = the (id, description) pairs list_skills returns. \
                 Instances are bulk-inserted, so nothing here measures the \
                 write path. Characters, not tokens: the tokeniser belongs to \
                 the report.",
        "latency_samples": LATENCY_SAMPLES,
        "rows": rows,
    });
    let rendered = serde_json::to_string_pretty(&out).unwrap();
    match std::env::var("ESCUREL_PAPER_DATA") {
        Ok(dir) => {
            std::fs::create_dir_all(&dir).ok();
            std::fs::write(format!("{dir}/tier1.json"), &rendered).expect("write");
        }
        Err(_) => println!("{rendered}"),
    }

    // The structural claim, asserted: the Tier-1 payload is byte-identical at
    // every scale. If this ever fails, `list_skills` has started depending on
    // the corpus and the paper's cost argument needs re-deriving.
    let first = rows[0]["tier1_chars"].as_u64().unwrap();
    for r in &rows {
        assert_eq!(
            r["tier1_chars"].as_u64().unwrap(),
            first,
            "Tier-1 payload moved with corpus size: {r}"
        );
    }
}
