# Rerank latency budget (#215)

What the cross-encoder rerank stage costs per `search` call, from the two
committed `escurel-eval` SciFact runs
([`baseline-scifact.md`](baseline-scifact.md), raw JSON alongside it).

## The budget model

The stage's worst case is bounded by **`rerank_candidates`** (default
**100**, `[retrieval].rerank_candidates` /
`ESCUREL_RETRIEVAL_RERANK_CANDIDATES`): the reranker scores at most that
many `(query, passage)` pairs per call, regardless of corpus size or the
caller's `k`. Per-pair cost is bounded too — the tokenizer truncates each
passage to the model's max sequence length — so:

```
rerank overhead ≈ rerank_candidates × cost(one CE forward pass)
```

Everything below is CPU-only candle (no BLAS), the pessimistic floor.

## Measured figures

| run | passages | candidates | first stage p50 | with rerank p50 |
|---|---|---|---|---|
| 1k baseline | 200-char snippets | 100 | 146 ms | **~15.1 s/query** |
| mini re-measure (#236) | full block bodies | 50 | 130 ms | **~63 s/query** |

- Snippet passages, 100 candidates: **~150 ms/pair** → ~15 s/query;
  concurrent throughput collapses to 0.2 QPS.
- Full-body passages (what production runs since #236), 50 candidates:
  **~1.3 s/pair** → ~63 s/query. Longer sequences dominate — halving the
  candidate count did not offset the passage growth.

Quality across the same runs: full-passage rerank is a genuine
improvement (nDCG@10 +0.018, MRR +0.031 vs single-pass within-run);
snippet rerank was a regression. Quality is settled; **latency is the
open blocker** for default-on rerank.

## Budget guidance

- **CPU serving: keep rerank off** (`[retrieval] rerank = "off"`, or a
  binary built without `--features rerank`). First-stage p50 is
  ~130–150 ms; any CPU rerank multiplies that by two to three orders of
  magnitude.
- **If rerank must run on CPU**, shrink the bound: `rerank_candidates`
  in the 10–20 range caps the worst case at roughly
  `candidates × ~1.3 s` (full-body pairs) — still seconds, tolerable
  only for non-interactive callers.
- **The real fix is per-pair cost**: GPU/BLAS inference and/or a lighter
  cross-encoder head. Until one of those lands, the default 100 is a
  quality-oriented setting for offline/eval use, not an interactive
  budget.

Re-measure with the harness (`escurel-eval`, this directory's README)
whenever the model, passage source, or device changes — the committed
runs above are the reference points.
