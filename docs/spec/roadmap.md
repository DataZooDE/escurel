# Roadmap — milestones, v1 cut-line, deferred items, licenses

## Milestones

The build proceeds inside-out: start at the storage layer, port
the indexer to match the Python prototype's behaviour exactly,
then wire transports, then live mode. Each milestone has a
concrete acceptance check.

### M1 — Storage + indexer (foundation)

Crates: `kb-storage`, `kb-md`, `kb-index`, `kb-embed` (without
runtime — stub embeddings first).

- `LaneStore` trait + `FsStore` implementation (dev-only).
- `S3Store` implementation — required for the substrate
  deployment target (S3 is the production default per locked
  decision 7). The substrate-target cut-line ships S3 as GA,
  not as a feature flag.
- Markdown parser (regex-based wikilink parser).
- Frontmatter parser (YAML; tolerant of the prototype's
  conventions).
- DuckDB schema migrations (`pages`, `links`, `blocks`,
  `crdt_ops`, `crdt_snapshots`, `frontmatter_index`).
- `vss` and `fts` extension load + HNSW + BM25 index creation
  on `blocks`.
- **Pre-deployment retrieval-quality spike** per
  [`../adr/0001-duckdb-only-storage.md`](../adr/0001-duckdb-only-storage.md):
  build the 460-block evaluation harness against DuckDB
  `vss` + `fts` + RRF fusion; run the 10,120-block
  synonym-mutant stress corpus; meet the acceptance table in
  the ADR's *Pre-deployment gate* section. Outcome gates M2
  work.
- `update_page`, `rebuild`, `audit` running end-to-end.

**Acceptance.** Port the 28-assertion e2e test from the Python
prototype to Rust. All 28 assertions PASS with timings within
2× of the prototype's.

### M2 — Embedding + retrieval

Crates: `kb-embed` proper, plus `search` / `resolve` / `expand`
/ `neighbours` / `list_skills` / `list_instances` /
`run_stored_query` over a direct Rust API (no transport yet).

- EmbeddingGemma loading via candle (CPU first; CUDA/Metal as
  feature flags).
- Gemini API adapter behind `embedding.provider = gemini`.
- Embed worker pool with the per-tenant queue.
- All seven read tools functional from a unit-test harness.

**Acceptance.** Agent-reproduction harness ports to Rust and
produces the same 3/3 task success at ≤ 1.5 k tokens.
EmbeddingGemma vs. the prototype's BGE-large quality difference
logged but not gating.

### M3 — Transports + auth + quotas

Crates: `kb-server` gateway, `kb-auth`, `kb-quota`, `kb-proto`.

- axum HTTP server with MCP/JSON-RPC framing.
- tonic gRPC server with the full `Kb` service.
- WebSocket endpoint (without live mode yet — just presence
  and search subscriptions stubbed out).
- OIDC verification with JWKS caching.
- Token-bucket quotas wired in.
- `kb` CLI as a thin MCP/HTTP + gRPC client.

**Acceptance.** Run the cold-start verification through the CLI
against a real running server. 8/8 queries correct under both
policies. Validate OIDC against a Keycloak test instance.

### M4 — Live CRDT mode + admin surface

Crates: `kb-crdt`, plus the admin half of `kb-server`.

- Loro adapter, `LiveDoc` actor per page, op log + snapshot
  persistence.
- WebSocket op streaming.
- `open_session` / `apply_op` / `close_session` over HTTP, WS,
  and gRPC bidi.
- Admin endpoints: tenant CRUD, export/import, attach_external,
  audit/rebuild streaming.
- Two-stage reconciler for external markdown edits.

**Acceptance.** Two CLI processes concurrently edit the same
page over WS; merged state is consistent on both sides.
Kill the server mid-session; on restart the op log replays
cleanly and the next `apply_op` succeeds.

### M5 — Observability + substrate deployment readiness + hardening

Crates: `kb-obs`. Plus substrate-binding artefacts ship in this
milestone (`S3Store` has moved to M1).

- OTel traces + metrics + JSON logs wired everywhere.
- `/metrics` Prometheus endpoint.
- Failure injection tests covering the recovery matrix from
  [`storage.md`](storage.md#crash-recovery-summary), including
  the cattle-node-loss → auto-rebuild-from-markdown path.
- License audit re-run; deps frozen.
- End-to-end deploy doc.
- **Substrate-target artefacts** (per
  [`../deploy/substrate.md`](../deploy/substrate.md)): Packer
  image fragment (candle libs + EmbeddingGemma model bake);
  Nomad jobspec set (`escurel-class` placement group, Vault
  template, OIDC env, Fabio `urlprefix-` tags); tenant-export
  shipper Nomad periodic job; Tailscale `tag:escurel` ACL
  fragment.

**Acceptance.** End-to-end smoke against three deploys:
single-binary on a laptop (FS, no OTLP); systemd unit on a VM
(FS, OTLP to local Tempo/Prometheus); **substrate `nonprod`**
(S3 LaneStore = Hetzner Object Storage, OTLP to substrate
collector, cattle-node loss → auto-rebuild). All three pass the
prototype's e2e verification suite.

### M6 — v1 ship

Cut a release. Tag `v1.0.0`. Publish operator docs.

## v1 cut-line — what is in vs. out

In:

- All 12 agent tools from
  [`../contract/agent-interface.md`](../contract/agent-interface.md)
- Live CRDT mode + whole-page fallback on all three transports
- **S3 LaneStore is the production default** (Hetzner Object
  Storage as the reference substrate target); local FS retained
  as a dev-only convenience
- EmbeddingGemma in candle (CPU default; CUDA/Metal flags)
- Gemini embeddings as an optional provider for environments
  that want hosted quality
- Three quota dimensions (queries, writes+embeds, concurrent
  sessions)
- Admin API: tenant CRUD, export/import, rebuild, audit,
  attach_external, embedding_reload, compact_db, quota_get,
  health
- OTel + JSON logs + `/metrics`
- `kb` CLI (operator + agent-style)
- Mandatory `kb` meta-skill shipped with every new tenant
- Event-typed skills supported via existing primitives;
  `at:` denormalised to an indexed column on both the DuckDB
  `pages` and `blocks` tables; `list_instances` accepts
  `order_by` and the operator-wrapped `FilterClause` syntax

Out (deferred):

- **Federation** across tenants
- **Live cursors**; v1 has presence badges only
- **Sidecar embedding adapter** (Ollama / TEI / vLLM) — the
  trait exists; concrete impl is post-v1
- **Web admin UI** — CLI only in v1
- **Reranker** beyond the small CE head bundled with
  EmbeddingGemma; bge-reranker-large as a `--features rerank`
  option only
- **Auto-provision-on-first-request** tenant flow
- **Live search subscriptions** (`search_subscribe` over WS) —
  schema reserved, off behind a feature flag
- **Event-derived state projection** (rules engine that maps
  events to state mutations automatically) — see below
- **Direct measurement at 1 M** instances; v1 extrapolates from
  100 k measured
- **Multi-model variance run** of agent harness benchmarks
  across GPT and Gemini

### Notes on deferred items

**Event-derived state projection.** v1 records events and state
side-by-side without deriving one from the other. An author or
agent who creates a `meeting` instance with a
`follow_ups: [[decision-record::expand-phoenix-scope]]` link is
expected to *also* create the decision-record. Automatic
projection (a rule: "when meeting commits with `follow_ups` X,
upsert decision-record X with `caused_by: meeting::Y`") is on
the roadmap as v1.5. The design implication today: events and
state are both *recorded*, not *derived*. A future projection
rules layer would sit between the indexer and the live mode and
emit synthetic writes.

**FTS retrieval quality.** FTS is the most stress-sensitive
retrieval path. A synonym-mutant stress corpus is part of the
pre-deployment gate in
[`../adr/0001-duckdb-only-storage.md`](../adr/0001-duckdb-only-storage.md),
with a declared 0.60 nDCG target. If tokenizer tuning (stemmer
/ k1 / b) cannot close the gap, the fallback is **not** to
revert the consolidation; it is to keep DuckDB for vector +
relational + CRDT and attach a separate external FTS engine
(e.g. Tantivy) for the lexical column only. v1 ships DuckDB
FTS as the default with a config flag (`retrieval.fts_backend =
"duckdb" | "external"`) that switches to the external-engine
path.

**FTS tokenizer tuning.** Stemming + k1 + b tuning on a more
realistic distractor distribution is the prerequisite to
making `fts_backend = "duckdb"` viable for all tenants.
Tracked as a research item under the consolidation gate.

## Licenses

The v1 dep set (Rust crates):

| crate | license | notes |
|---|---|---|
| `object_store` | Apache-2.0 / MIT | from the Apache Arrow project |
| `duckdb` (Rust bindings) | MIT | bindings; DuckDB itself MIT; the `vss` and `fts` extensions are part of the DuckDB extension ecosystem under MIT |
| `loro` | MIT | CRDT |
| `candle-core`, `candle-nn`, `candle-transformers` | MIT/Apache-2.0 | HF inference runtime |
| `tonic`, `prost` | MIT | gRPC |
| `axum`, `hyper`, `tokio` | MIT | HTTP + runtime |
| `tower`, `tower-http` | MIT | middleware |
| `tracing`, `tracing-opentelemetry` | MIT | logs + traces |
| `opentelemetry`, `opentelemetry-otlp` | Apache-2.0 | OTel SDK |
| `prometheus` | Apache-2.0 | scrape endpoint |
| `jsonwebtoken` | MIT | JWT verification |
| `reqwest` | MIT/Apache-2.0 | HTTP client (Gemini adapter) |
| `serde`, `serde_json`, `serde_yaml`, `toml` | MIT/Apache-2.0 | (de)serialisation |
| `clap` | MIT/Apache-2.0 | CLI |
| `dashmap`, `lru` | MIT | data structures |
| `ulid` | MIT | page id generation |
| `regex`, `aho-corasick` | MIT/Apache-2.0 | wikilink parsing |

Permissive across the board. No GPL surface. Closed-source SaaS
operators can ship this without surfacing source. Embedding
model weights (`google/embeddinggemma-300m`) are governed by
the Gemma license, which permits commercial use including
hosted services subject to the prohibited-use policy — operators
should review independently if their use case is unusual.

## After v1

The shape of the v1.5 / v2 directions:

1. **Event-derived state projection** — a rules-engine layer
   that materialises supersession / chain links from event
   `follow_ups` fields. Optional per-tenant; off by default
   (the absence of automation is part of the v1 contract).
2. **Live cursors and OT-grade collaboration** — once the
   dependent libraries stabilise.
3. **Federation** — query routing across tenants for an admin
   asking cross-tenant questions; behind a permission layer.
4. **Sidecar embedding adapter** — TEI / vLLM / Ollama; gives
   GPU-resource-constrained operators a way to pool inference
   capacity.
5. **Reranker upgrade** — bge-reranker-large by default; the CE
   head from EmbeddingGemma stays as the fast-path.
6. **Web admin UI** — replaces the CLI for everyday operator
   tasks. The CLI stays for scripting.
7. **Multi-region S3 backends** — read-replica datasets for
   geographic distribution; the writer remains single-region.
8. **Substrate-target package** — Terraform module + Nomad
   jobspec set + Packer image fragment published as release
   artefacts, so substrate operators consume `kb-server` as a
   turnkey workload. Per-target binding docs grow as new
   substrate targets are adopted (managed-K8s, single-VM, etc.).

None of these break the v1 contract. The Rust crate layout was
chosen with these extensions in mind: each lives in a new crate
that depends on the existing ones, not in a refactor.
