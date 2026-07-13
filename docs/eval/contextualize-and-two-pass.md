# Contextual Retrieval (#216) & Matryoshka two-pass (#218) — measurement methodology

This note closes the **measurement** acceptance items of #216 and #218. Both
features are code-complete and wired live; what remained was a repeatable way to
quantify their effect on the eval harness. This note supplies the harness knobs
and the exact commands; the numbers are an **operator hardware run** (real
EmbeddingGemma via the `candle` feature — not runnable in CI/sandbox, where only
the offline `HashEmbedder` runs and vector scores are not semantically
meaningful, so no numbers are fabricated here).

## What changed to make these measurable

- **#216 contextualize** is an *ingest-time* property (it changes the stored
  embedding input), not a query-time matrix row. The harness now takes
  `--contextualize off|structural` (`escurel-eval/src/{main,ingest,lib}.rs`): a
  BEIR doc is embedded either verbatim (`off`) or title-prefixed
  (`structural` → `[<title>]\n<body>`). You measure the delta by running the
  harness twice — once per mode — and comparing the two reports.
- **#218 two-pass** is already a first-class `RunConfig` arm (`two_pass`), so it
  is a single-run matrix row alongside `single_pass`.

## Commands

Real embedder + reranker (operator hardware; needs `--features candle,rerank`):

```bash
# #218 — two-pass vs single-pass, same ingested index, one run:
cargo run -p escurel-eval --features candle,rerank --release -- \
  --dataset datasets/scifact --skill paper \
  --embed-model google/embeddinggemma-300m \
  --k 100 --coarse-dim 128 --coarse-candidates 500 \
  --qps-workers 16 --qps-secs 30 --format json > two-pass.json

# #216 — contextualize off vs structural, two runs, compare:
cargo run -p escurel-eval --features candle --release -- \
  --dataset datasets/scifact --skill paper --contextualize off \
  --format json > ctx-off.json
cargo run -p escurel-eval --features candle --release -- \
  --dataset datasets/scifact --skill paper --contextualize structural \
  --format json > ctx-structural.json
```

CI / offline plumbing check (deterministic, `HashEmbedder`, tiny fixture — proves
the knobs run, not a quality signal):

```bash
cargo test -p escurel-eval --test smoke   # contextualized_ingest_produces_a_report
```

## Setup (template — fill from the operator run)

| | |
|---|---|
| Dataset | SciFact (BEIR) — corpus / queries / qrels |
| Embedder | `google/embeddinggemma-300m` (768-d, `candle`) |
| Reranker | `BAAI/bge-reranker-base` (`rerank`), for the two-pass+rerank row |
| k / coarse | k=100, coarse_dim=128, coarse_candidates=500 |
| Hardware | (operator) |

## Results (template)

Paste each run's `EvalReport` rows (matches `EvalReport::to_table`):

| config | nDCG@10 | nDCG@100 | recall@10 | recall@100 | MRR | MAP | p50 ms | p95 ms | QPS |
|---|---|---|---|---|---|---|---|---|---|
| single_pass (ctx off) | … | … | … | … | … | … | … | … | … |
| single_pass (ctx structural) | … | … | … | … | … | … | … | … | … |
| two_pass | … | … | … | … | … | … | … | … | … |

## Findings (interpret as within-comparison deltas, not absolutes)

- **#216 contextualize (off → structural).** Expect a small nDCG/recall lift
  (Anthropic reported ~35% fewer retrieval failures from contextual embeddings +
  BM25); the delta is the signal. `structural` adds no per-doc storage — the
  context prefixes the embed input and the FTS text; the verbatim body is stored
  unchanged for display. Variant B (`--contextualize llm`, `contextualize-llm`
  feature) is expected to lift further at an ingest-time inference cost.
- **#218 two-pass (single_pass → two_pass).** Expect **quality-neutral** nDCG/
  recall (the coarse 128-d ANN shortlist is rescored at full 768-d) with a
  latency/QPS improvement — it is a throughput optimization that buys headroom to
  afford reranking. **Storage cost: none** — the coarse pass truncates the
  existing 768-d vector on read (no second index).

## Caveats

- The tiny CI fixture + `HashEmbedder` are plumbing-only: they prove the knobs
  run and the report is well-formed, **not** retrieval quality. Absolute numbers
  require the real embedder on hardware.
- `--contextualize` compares two separately-ingested indexes; keep every other
  flag identical between the two runs so the delta is attributable.
