# Baseline — BEIR SciFact (1k subsample)

A committed `escurel-eval` run, produced by:

```bash
escurel-eval --dataset datasets/scifact-1k --skill paper \
  --embed-model BAAI/bge-base-en-v1.5 --reranker BAAI/bge-reranker-base \
  --k 100 --coarse-dim 128 --coarse-candidates 500 \
  --qps-workers 8 --qps-secs 8 --format json
```

Raw JSON: [`baseline-scifact.json`](baseline-scifact.json).

## Setup

| | |
|---|---|
| Dataset | BEIR SciFact, **1000-doc qrels-preserving subsample** (all 283 test-judged docs + 717 distractors) |
| Queries | 300 (the SciFact test split) |
| Embedder | `BAAI/bge-base-en-v1.5` (768-d BERT) |
| Reranker | `BAAI/bge-reranker-base` (XLM-RoBERTa cross-encoder) |
| `k` / coarse | k=100, coarse_dim=128, coarse_candidates=500 |
| Hardware | CPU only (candle, no BLAS) |

**Why a 1k subsample, not the full 5183-doc corpus:** candle CPU BERT embedding
(no BLAS in the default build) runs at ~0.5 docs/s, so the full corpus is hours
of ingest. The subsample keeps every judged doc, so recall/nDCG are well-defined;
it is an easier corpus than the full set, so treat the **absolute** numbers as
indicative and the **per-config deltas** as the signal. The harness runs the full
corpus unchanged on a BLAS/GPU build.

## Results

| config | nDCG@10 | nDCG@100 | recall@10 | recall@100 | MRR | MAP | p50 ms | p95 ms | QPS |
|---|---|---|---|---|---|---|---|---|---|
| single_pass      | **0.846** | 0.863 | 0.921 | 0.993 | 0.831 | 0.822 | 146 | 183 | 12.5 |
| two_pass         | 0.847 | 0.864 | 0.924 | 0.993 | 0.831 | 0.823 | 174 | 213 | 10.0 |
| rerank           | 0.671 | 0.710 | 0.802 | 0.993 | 0.644 | 0.632 | 15104 | 21418 | 0.2 |
| two_pass_rerank  | 0.672 | 0.711 | 0.803 | 0.993 | 0.645 | 0.632 | 15030 | 21039 | 0.2 |

(p50/p95 are the sequential per-query latency; QPS is the 8-worker concurrent
pass — the `Indexer` connection mutex serializes DuckDB, so this is single-writer
throughput.)

## Findings

**#218 two-pass — quality-neutral, small latency cost (as designed).**
`single_pass` → `two_pass`: nDCG@10 +0.001, recall@10 +0.003 (noise), p50 +28 ms.
The coarse 128-d prefix shortlist (500 of 1000 docs) preserves the full-dim
ranking here. The latency *increase* is expected: the truncate-on-read coarse
pass is a cheaper-per-row scan, **not** a low-dim ANN index, so on this corpus
size it adds work rather than saving it — exactly the trade-off the #218 PR
documented (a second 128-d HNSW index is the throughput win, deferred). Two-pass
pays off at corpus sizes where the full-dim HNSW scan dominates, not at 1k docs.

**#215 rerank — regresses quality AND latency here. Two real causes:**

1. **Quality drop (nDCG@10 0.846 → 0.671).** bge-base single-pass is already a
   strong retriever on SciFact, and the rerank stage scores the **200-char block
   snippet**, not the full abstract (`rerank_passage` uses `SearchHit.snippet`,
   the hydrated lead — a deliberate latency choice in the #215 stage PR). On
   abstract-length docs the cross-encoder sees ~13% of the passage and reorders
   *worse* than the bi-encoder that embedded the whole doc. **Actionable:** feed
   the reranker fuller passage text (refetch the block `body`), at least for
   document/RAG skills.
2. **Latency (~15 s/query sequential, QPS 0.2).** A CPU cross-encoder scoring 100
   `(query, passage)` pairs per query is ~15 s; concurrent throughput collapses
   to 0.2 QPS. **Actionable:** rerank only makes sense on GPU, and/or with a much
   smaller `rerank_candidates` (e.g. 20–50), and/or a lighter CE head.

The harness did its job: it turned "the reranker is wired in" into a measured,
falsifiable result — on this benchmark, the rerank stage as **originally**
configured (snippet passages) was a net negative, and the report pointed at the
two concrete levers to change that.

## Update — full-passage rerank (#236)

Lever #1 ("feed the reranker the full body, not the snippet") landed in #236:
`rerank_hits` now bulk-fetches `blocks.body` for the candidate set. Re-measured
on a smaller, faster mini split (400 docs, the first 50 test queries, k=50,
`rerank_candidates=50`, full passages — raw:
[`remeasure-fullpassage-scifact-mini.json`](remeasure-fullpassage-scifact-mini.json)):

| config | nDCG@10 | recall@10 | MRR | MAP | p50 ms |
|---|---|---|---|---|---|
| single_pass     | 0.9166 | 0.980 | 0.9018 | 0.8930 | 130 |
| two_pass        | 0.9166 | 0.980 | 0.9018 | 0.8930 | 143 |
| rerank          | **0.9345** | 0.956 | **0.9324** | **0.9299** | 63064 |
| two_pass_rerank | 0.9345 | 0.956 | 0.9324 | 0.9299 | 63523 |

**The regression flips to an improvement.** Within-run, rerank vs single_pass:

| metric | snippet (1k run above) | full passage (#236) |
|---|---|---|
| ΔnDCG@10 | **−0.175** | **+0.018** |
| ΔMRR | −0.19 | **+0.031** |

(Absolute numbers differ from the 1k run — different corpus/queries/k — so the
**within-run delta** is the signal; it went from clearly negative to positive.
The cross-encoder now improves ranking, as expected.)

**Latency got worse, not better** (~63 s/query, even with half the candidates):
full passages are longer token sequences, so each `(query, passage)` pair costs
more than a snippet pair. This sharpens lever #2 — rerank on CPU is impractical
at any useful candidate count; it needs GPU and/or a small `rerank_candidates`
and/or a lighter CE head. Quality is fixed; **latency is the remaining blocker**
for default-on rerank.

## Caveats

- Absolute nDCG is on a 1k subsample (easier than full SciFact) and is **not** the
  ADR-0001 460-block target — those numbers await escurel's own corpus in this
  same BEIR format (`docs/eval/README.md`).
- CPU-only; GPU / BLAS would change the latency picture (and make the full corpus
  + rerank tractable).
