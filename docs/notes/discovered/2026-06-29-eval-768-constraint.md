# Eval embedder must be 768-d AND a BERT-family model

Two independent constraints pin which embedding model `escurel-eval` (or any
harness building an `Indexer`) can use.

## 1. 768 dimensions

**Symptom.** `Indexer::new` fails with `EmbedderDimMismatch { expected: 768,
got: N }` for any non-768-d model (e.g. `sentence-transformers/all-MiniLM-L6-v2`
is 384-d — the model the in-repo candle smoke test uses).

**Cause.** `blocks.dense_vec` is declared `FLOAT[768]`
(`BLOCKS_DENSE_VEC_DIM = 768`, `crates/escurel-index/src/indexer.rs`) and
`Indexer::new` validates `embedder.dim() == 768` up front. The dimension is
hard-wired into the schema; there is no per-tenant dim today.

## 2. BERT architecture

**Symptom.** Loading `google/embeddinggemma-300m` (the model named throughout
the spec + `escurel-server`'s `embeddinggemma` feature) fails: its `config.json`
is a Gemma3 config, not a BERT one, so `CandleEmbedder::from_local` cannot build
a model from it.

**Cause.** `CandleEmbedder` is built on
`candle_transformers::models::bert::BertModel`
(`crates/escurel-embed/src/candle.rs`). candle-transformers (0.9) ships no
gemma3 *sentence-encoder* path, so EmbeddingGemma is **aspirational** — the
candle.rs doc comment even says "EmbeddingGemma once gemma3 lands in
candle-transformers". (The `escurel-server` `embeddinggemma` feature inherits
this: that path is not actually runnable today.)

## What to use

A 768-d **BERT** retrieval encoder. The eval pins **`BAAI/bge-base-en-v1.5`**
(public, 768-d, BERT, strong retrieval, same family as `bge-reranker-*`).
Confirmed loading + embedding via `CandleEmbedder::from_hf_hub(repo, 768)`.

**How to recognize it.** A dim error → wrong dimension; a "build BertModel" /
config-parse error on a Gemma/Llama/MPNet repo → wrong architecture. Pass
`expected_dim = 768` so the dim mismatch surfaces at load time. Evaluating a
different-dimension model needs a schema change, not a config flag.
