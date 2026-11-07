# Model providers: the runner harness and embeddings

The runner's model and the gateway's embedder are configured **independently**
per deployment. A provider that serves chat need not serve embeddings (Hetzner
does not), and the two often have different contracts, regions and keys.

The generated variable table is [`env.md`](env.md); this page says which
variables to set for which provider, and how to choose.

## Runner harness (`ESCUREL_RUNNER_HARNESS`)

Five in-process HTTP harnesses — the ones a container can run. Each preset is a
selector value, so a run's `harness` field and `ESCUREL_RUNNER_HARNESS_ALLOW`
name the provider that ran it.

| selector | provider | endpoint default | ordinary model | long-run model | streaming | tool calling | key |
|---|---|---|---|---|---|---|---|
| `alibaba` | Alibaba Cloud Model Studio, OpenAI-compatible mode | `https://dashscope.aliyuncs.com/compatible-mode/v1` (China, Beijing) | `qwen3.8-flash` | `qwen3.8-max` | on | `native` (documented) | `ESCUREL_RUNNER_ALIBABA_API_KEY_FILE` |
| `hetzner` | Hetzner Inference API (experimental) | `https://inference.hetzner.com/api/v1` | `Qwen3.8-27B` | the ordinary model | off | `probe` | `ESCUREL_RUNNER_HETZNER_API_KEY_FILE` |
| `openai-compatible` | any `/chat/completions` endpoint | none — set `ESCUREL_RUNNER_OPENAI_BASE_URL` | none — set `ESCUREL_RUNNER_OPENAI_MODEL` | the ordinary model | off | `probe` | `ESCUREL_RUNNER_OPENAI_API_KEY_FILE` |
| `gemini` | Gemini API | Generative Language API | `gemini-3.8-flash` | — | — | native | `ESCUREL_GEMINI_API_KEY` |
| `gemini-vertex` | Vertex AI, EU, workload identity | regional EU endpoint | `gemini-3.5-flash` | — | — | native | none (metadata server) |

Every OpenAI-compatible preset reads the same eight variables under its own
prefix (`ESCUREL_RUNNER_ALIBABA_*`, `ESCUREL_RUNNER_HETZNER_*`,
`ESCUREL_RUNNER_OPENAI_*`):

| suffix | meaning |
|---|---|
| `_BASE_URL` | base URL (`…/v1`); `https`, or plain `http` to a loopback address only |
| `_API_KEY_FILE` | **file** holding the API key — a mounted secret. There is no key-in-env variable |
| `_MODEL` | model for ordinary runs (events, manual starts, chat) |
| `_LONG_MODEL` | model for long, multi-step runs — workflow steps |
| `_STREAM` | `true` / `false` |
| `_TOOLS` | `native` or `probe` (see below) |
| `_REASONING_EFFORT` | passed through as `reasoning_effort` |
| `_MAX_TOTAL_TOKENS` | provider-reported token cap per invocation (default 250000) |

The adapter's own wall clock follows the runner's (`ESCUREL_RUNNER_RUN_TIMEOUT`,
`ESCUREL_RUNNER_CHAT_TIMEOUT`), so a long-model workflow step is bounded by the
runner, not by a smaller adapter default. Shared by the three: `ESCUREL_RUNNER_CHAT_COMPLETIONS_REQUEST_TIMEOUT` (180s, one request
including its stream), `ESCUREL_RUNNER_CHAT_COMPLETIONS_MAX_RETRIES` (3) and
`ESCUREL_RUNNER_CHAT_COMPLETIONS_RETRY_BACKOFF` (1s, doubled per retry, `Retry-After` wins,
capped at 30s). `ESCUREL_RUNNER_MAX_TURNS` bounds the model turns as for Gemini.

### Which model a run uses

By priority:

1. the skill page's `model:` — within `ESCUREL_RUNNER_MODEL_ALLOW`, else the run fails closed;
2. a **chat** run: `ESCUREL_RUNNER_CHAT_MODEL`, when set;
3. a **workflow step** (a run carrying `provenance.workflow`, the runner's
   multi-step kind): the preset's `_LONG_MODEL`, else its default long model;
4. otherwise the preset's `_MODEL`, else its default.

Steps 2–4 are operator settings and need no allow-list entry. "Long" is a
property of the run kind, not of a measured duration: a single event-triggered
run that happens to take many turns stays on the ordinary model. Gemini has no
long-model slot.

### Behaviour that matters operationally

- **Misconfiguration refuses to start** (exit 2): a missing or empty key file,
  no base URL or model where the preset has no default, a non-https endpoint.
  Checked for the configured harness and for every preset in
  `ESCUREL_RUNNER_HARNESS_ALLOW`. It never falls back to `echo`.
- **Tool calling is not assumed where it is not documented.** With `probe`,
  the first run sends one small request declaring a single tool
  (`escurel_tool_probe`, `tool_choice: auto`). A model that does not call it,
  or an endpoint that rejects `tools` with a 4xx, makes the run fail
  **permanently** with a message naming the endpoint and model. The verdict is
  A positive verdict is
  cached per endpoint and model for the life of the process; a negative one is
  not (one prose answer under `tool_choice: auto` may be a whim), so the next
  run probes again. Set `_TOOLS=native` only when the provider documents tool
  calling.
- **Reasoning content** (`reasoning_content`, `reasoning`, inline
  `<think>…</think>`) never reaches the reply or the run summary. On `alibaba`
  it is sent back on assistant turns within a run, as Model Studio requires
  for Qwen 3.8; on the other presets it is dropped.
- **Retries:** 429, 500, 502, 503, 504 and refused connections only. A 400 or
  401 is reported at once with the provider's message. A run whose model call
  failed after retries, or timed out, is not retried by the reconciler: its
  usage is unknown (the same rule as Gemini).
- **Bounded responses:** at most 64 tool calls and 8 MiB per turn; a stream
  that ends before `[DONE]` or a `finish_reason` is an upstream error, not a
  finished turn. Provider error text is relayed with the API key redacted.
- **Usage is required.** A response without `usage` fails the run, because the
  token budget cannot be checked. Streaming requests ask for it with
  `stream_options.include_usage`.

## Embeddings (`ESCUREL_EMBEDDING_PROVIDER`, gateway)

| provider | endpoint default | model default | key | notes |
|---|---|---|---|---|
| `gemini` | Generative Language API | `gemini-embedding-001` | `ESCUREL_GEMINI_API_KEY` | no key: zero vectors (semantic search off) |
| `alibaba` | `https://dashscope.aliyuncs.com/compatible-mode/v1` (set the workspace URL via `ESCUREL_EMBEDDING_OPENAI_BASE_URL`) | `text-embedding-v4` | `ESCUREL_EMBEDDING_OPENAI_API_KEY_FILE` | 10 texts per request; `dimensions` sent |
| `openai-compatible` | none — set `ESCUREL_EMBEDDING_OPENAI_BASE_URL` | none — set `ESCUREL_EMBEDDING_MODEL` | `ESCUREL_EMBEDDING_OPENAI_API_KEY_FILE` | 16 per request (`ESCUREL_EMBEDDING_OPENAI_BATCH`); `ESCUREL_EMBEDDING_OPENAI_SEND_DIMENSIONS=false` for a model that rejects `dimensions` |
| `embeddinggemma` | local (candle) | `BAAI/bge-base-en-v1.5` | none | air-gapped; feature build only |
| `zero` | — | — | — | development only |

`hetzner` is refused as an embedding provider: the Hetzner Inference API has
no embeddings endpoint. A deployment that runs its chat on `hetzner` embeds
with one of the others.

The dimension is fixed at 768 (`ESCUREL_EMBEDDING_DIM`); `text-embedding-v4`
supports it. A missing key file is a load failure: the gateway boots degraded
(semantic search off, `/readyz` reports the embedder) or, with
`ESCUREL_EMBEDDER_REQUIRED=true`, refuses to boot. The key travels as a bearer
header only and is redacted from errors.

**The embedding model is part of the index's identity.** The index records
the embedder's model id — `alibaba:text-embedding-v4`, or
`openai-compatible@<host>:<model>` for a generic endpoint, since the same model
name at two hosts is not promised to be the same model — and the dimension.
At boot, and on `embedding_reload`, it is brought into the configured model's
space:

- **another model recorded, or vectors of unrecorded origin** (an index from
  before this record existed — the first boot after upgrading pays one
  re-embed): every block is re-embedded and local chat-message vectors are
  cleared. Vectors from two models are never mixed;
- **the same model**: only blocks holding zero vectors — written while the
  embedder was degraded — are re-embedded;
- **degraded** (the zero placeholder): nothing is rewritten.

A re-embed never truncates. New vectors are staged per batch and swapped in,
with the new record, in one transaction once every block has one. A provider
failure half way leaves the old vectors serving; the boot then continues
degraded (or refuses, with `ESCUREL_EMBEDDER_REQUIRED=true`), and the next
attempt resumes from the staged batches. The pass holds the write lock, so
writes wait for it; `embedding_reload` activates the new model only after the
re-embed succeeded and blocks until then. Chat vectors in a shared table
(Postgres, the lake) are not cleared by a replica; the log says so. A DuckLake
reader refuses a lake whose manifest names another model, as before: publish
from the re-embedded writer, then point the readers at the new model.

A tenant spec (`tenant.json`) may select `alibaba` or `openai-compatible` only
when `ESCUREL_EMBEDDING_OPENAI_BASE_URL` is set in the environment: a spec does
not get to send page bodies to a provider default the operator never named.

## How to choose

Decide chat and embeddings separately; the questions differ.

**Chat (runner).**

| if | choose | because |
|---|---|---|
| data must stay in the EU and Gemini is acceptable | `gemini-vertex` | EU endpoint, workload identity, no key to rotate |
| data must stay in Germany/EU and an experimental service is acceptable | `hetzner` | Hetzner states it stores usage data only, not content; but rate limits are enforced, the model list changes, and tool calling must pass the probe |
| cost or Qwen model quality decide and processing in China (Beijing) is acceptable | `alibaba` | documented tool calling and streaming; two-tier flash/max models |
| another vendor or a self-hosted vLLM | `openai-compatible` | nothing assumed; probe verifies tool calling |

Check the provider's terms before selecting a subscription key. Alibaba's
**Token Plan** (`https://token-plan.cn-beijing.maas.aliyuncs.com/compatible-mode/v1`,
keys `sk-sp-…`) is documented for interactive use in coding and agent tools
and excludes custom application backends and non-interactive batch calls; a
runner is a backend. Use a pay-as-you-go Model Studio key and the workspace
endpoint `https://{WorkspaceId}.cn-beijing.maas.aliyuncs.com/compatible-mode/v1`
for a deployment unless the provider confirms otherwise.

**Embeddings (gateway).** Vectors from different models are not comparable.
Changing the embedding model re-embeds the corpus, at the provider's price per
token; choose once per tenant and change deliberately. Where data residency
decides the chat provider, it decides the embedding provider too: every page
body is sent to it. Whether Alibaba's Token Plan covers `text-embedding-v4` is
not documented (its model list names no embedding model); assume it does not
and use a pay-as-you-go key.
