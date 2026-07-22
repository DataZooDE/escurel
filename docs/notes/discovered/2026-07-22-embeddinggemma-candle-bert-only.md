# EmbeddingGemma can't load on the candle backend (BERT-only) → silent degraded boot

**Date:** 2026-07-22
**Issue:** #299

## Symptom

Following the documented local/air-gapped default —

```sh
ESCUREL_EMBEDDING_PROVIDER=embeddinggemma
ESCUREL_EMBEDDING_MODEL=google/embeddinggemma-300m
ESCUREL_EMBEDDING_DIM=768
```

— the gateway boots but retrieval is degraded: `embedder_loaded: false`,
zero-vector embeddings, FTS-only search. Forcing a reload surfaces the real
reason:

```
model load failed: parse config.json: missing field `hidden_act` at line 60
```

The only user-facing signal at boot was a single `tracing::warn!` line, which
is easy to miss. `/readyz` did return 503 (`embedder: false`), but the server
still served traffic.

## Cause

`crates/escurel-embed/src/candle.rs` loads
`candle_transformers::models::bert::BertModel` — a **BERT-family** loader.
`google/embeddinggemma-300m` is `model_type: gemma3_text`, a different
architecture: its `config.json` uses `hidden_activation` (not BERT's
`hidden_act`), and even past the config parse the weights would not run as a
BERT. candle-transformers has no Gemma3 embedding path yet.

## Fix

1. Changed the candle default model (`config.rs::load_embeddinggemma`) from
   `google/embeddinggemma-300m` to `BAAI/bge-base-en-v1.5` — a 768-dim
   BERT-family sentence-transformer that loads cleanly on the existing path.
   Docs (`docs/deploy/README.md`, `docs/spec/README.md`, the config env
   table) updated to match.
2. Added `ESCUREL_EMBEDDER_REQUIRED` (default `false`): when `true`, a failed
   real-embedder load aborts the boot instead of silently degrading. The
   degrade-then-reload baseline stays the default so keyless dev/CI boots and
   air-gapped recovery via the `embedding_reload` admin RPC keep working.

The `embeddinggemma` provider name is retained (it selects the candle path);
EmbeddingGemma proper returns as the default once `gemma3` lands in
candle-transformers.

## How to recognise it next time

Any candle-backed embedder that isn't a BERT sentence-transformer will fail
the same way — a `config.json` parse error naming a missing BERT field
(`hidden_act`), or a load failure past it. If a boot logs
`embedder failed to load; booting degraded`, check the model architecture
against the BERT-only loader before assuming a missing-weights / network
problem. Set `ESCUREL_EMBEDDER_REQUIRED=1` to turn the silent degrade into a
loud boot failure.
