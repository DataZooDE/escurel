# Retrieval evaluation harness (`escurel-eval`)

`escurel-eval` measures escurel's retrieval quality (nDCG / recall / MRR / MAP)
and performance (latency p50/p95/p99 + concurrent QPS) by running
`Indexer::search` over a labeled IR dataset under each retrieval configuration.
It exists to answer the acceptance questions left open by the RAG work:

- **#215** — the rerank-on-vs-off quality delta (`single_pass` vs `rerank`).
- **#218** — the two-pass recall + QPS delta vs single-pass (`single_pass` vs
  `two_pass`), and whether two-pass pays for the rerank latency
  (`two_pass_rerank` vs `rerank`).
- **ADR-0001 §Pre-deployment gate** — nDCG / p50 / p95 targets (via `--gate`).

## What it does

1. Loads a **BEIR-format** dataset: `corpus.jsonl {_id,title,text}`,
   `queries.jsonl {_id,text}`, `qrels/test.tsv (query-id\tcorpus-id\tscore)`.
   The corpus `_id` is used verbatim as the escurel `page_id`, so qrels compare
   directly against search hits.
2. Embeds + indexes the corpus **once** into a persistent DuckDB (via the
   embed-free `write_document_blocks`), then reuses that one index/connection
   for every config — no re-embedding, no second DuckDB handle.
3. Runs the config matrix — `single_pass`, `two_pass`, `rerank`,
   `two_pass_rerank` — and reports per-config metrics + latency + QPS as a table
   or JSON.

The metric, dataset, and report layers are pure and offline; CI exercises the
whole loop with the deterministic `HashEmbedder` over a tiny fixture
(`tests/smoke.rs`). The real model paths are feature-gated and run by hand.

## Embedder constraints: 768-d **and** BERT-family

Two constraints pin the eval encoder:

- `blocks.dense_vec` is `FLOAT[768]` and `Indexer::new` rejects any embedder
  whose `dim() != 768` — so it must be 768-dimensional (a 384-d model like MiniLM
  is rejected).
- `CandleEmbedder` loads **BERT-architecture** models (`BertModel`).
  `google/embeddinggemma-300m` is Gemma3 and is **not** loadable today (candle-
  transformers has no gemma3 sentence-encoder path yet).

So the eval uses a 768-d **BERT** retrieval encoder — **`BAAI/bge-base-en-v1.5`**
(public, strong, same family as the bge reranker). See
`docs/notes/discovered/2026-06-29-eval-768-constraint.md`.

## Fetch a dataset (SciFact)

BEIR datasets are on the Hugging Face hub in exactly this layout. SciFact
(~5183 docs / 300 test queries) is small enough for a CPU run:

```bash
huggingface-cli download BeIR/scifact --repo-type dataset --local-dir datasets/scifact
# the canonical BEIR zip is an alternative:
#   https://public.ukp.informatik.tu-darmstadt.de/thakur/BEIR/datasets/scifact.zip
```

You should end up with `datasets/scifact/{corpus.jsonl,queries.jsonl,qrels/test.tsv}`.
Datasets are **not** committed.

## Run

```bash
cargo run -p escurel-eval --features candle,rerank --release -- \
  --dataset datasets/scifact --skill paper \
  --embed-model BAAI/bge-base-en-v1.5 \
  --reranker BAAI/bge-reranker-base \
  --k 100 --coarse-dim 128 --coarse-candidates 500 \
  --qps-workers 16 --qps-secs 30 \
  --format table
```

First run downloads the embedding + reranker models into the HF cache and embeds
the corpus (minutes on CPU); the index is persisted under `<dataset>/.eval/`, so
add `--skip-ingest` to re-query the same index without re-embedding.

Without `--features candle` the harness falls back to `HashEmbedder` (results are
**not** semantically meaningful — plumbing only); without `--features rerank` the
rerank configs are skipped.

## Gate (optional, manual)

`--gate thresholds.txt` compares the report against a flat `key = value` file and
exits non-zero on any failure:

```
# thresholds.txt — applied to every config
min_ndcg_at_10    = 0.60
min_recall_at_100 = 0.90
max_p50_ms        = 15
max_p95_ms        = 50
min_qps           = 100
```

## Interpreting the numbers

The harness reports **deltas** that are directly comparable on a fixed dataset
(rerank on/off, two-pass vs single). The **absolute** nDCG on SciFact is *not*
the ADR-0001 0.95/0.90/0.60 figures — those are tied to escurel's own (not yet
in-repo) 460-block corpus. When that corpus lands in this same BEIR format, it
drops into the loader unchanged and the `--gate` thresholds become the ADR gate.

See `baseline-scifact.md` for a committed run, and
`rerank-latency-budget.md` for what the #215 rerank stage costs per query
and how `rerank_candidates` bounds the worst case.
