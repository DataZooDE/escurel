# Paper: "Two Dualities" — annotated outline + measurement specification

**Status:** plan, 2026-08-03 (rev. 2 — predecessor results folded in;
backend polymorphism elevated to the thesis).
**Deliverable of this document:** the outline *plus* the measurement
specification. Every table and figure below is specified with its exact
metric, its exact configuration, and the harness invocation that must
produce it. The harness work is scoped here but not executed.

**Target:** 8–10 page PVLDB experience/industrial-track paper, built with
the same toolchain as `duckdb-gdrive/docs/paper/` (acmart `sigconf`,
vendored under `cls/`, `make` prints the page count).

---

## Context

### What escurel is, in one paragraph

escurel is a knowledge base for agents. A **skill** is a markdown page
that is simultaneously a *type* (the frontmatter schema a class of pages
must satisfy) and *prose instructions* an LLM reads. An **instance** is a
page that declares which skill it conforms to, and is simultaneously a
*row* of that type and a *document* a human edits in Obsidian. Crucially,
**an instance's body need not be markdown**: the skill declares a
`backend:` — `markdown` (CRDT-backed, writable), `sql_view` (a read-only
DuckDB view over Postgres / MySQL / SQLite / erpl / json\_dir /
parquet\_dir), `document` (an uploaded PDF/DOCX/PPTX/XLSX, extracted and
embedded), or the two **live remote proxies** `openapi` and `mcp`, whose
data is fetched on every `expand` from a REST endpoint or an upstream MCP
server and which accept **write-back** upstream. Typed `[[skill::id]]`
wikilinks are validated at index time and may pin a version
(`[[table::customers.churn@v14]]`). An **event** is an immutable fact
landing in an inbox; a **callback** is an agent-harness run, triggered by
that event and *instructed by the event's `label_skill` page*, that folds
it into instance state. Storage is one DuckDB file per tenant carrying
relational metadata, HNSW vectors, BM25, the CRDT op log and an attached
DuckLake catalog, with `pages/` markdown as canonical source of truth.
The wire surface is MCP-over-HTTP plus WebSocket.

### What already exists in this repo

| asset | what it gives the paper |
|---|---|
| `docs/contract/agent-interface.md` (636 lines) | tool surface, tier model, design principles, locked decisions |
| `docs/contract/agent-orchestration.md` (349) | the event→callback loop: runner, webhook, ledger, cascade controls |
| `docs/contract/dynamic-workflows.md` (741) | the argument that a workflow is the *same* loop with a generalised emit policy — §4.5 |
| `docs/contract/compile-first-wiki.md` (328) | self-maintaining-KB operations (out of scope; one paragraph in §7) |
| `docs/spec/protocol.md` §Instance backends + §Remote backends | the five-substrate model, its security invariants, the tool surface for it |
| 11 ADRs | one locked decision each, with alternatives and consequences already written |
| `docs/eval/` + `crates/escurel-eval/` | working BEIR-format retrieval harness (nDCG / recall / MRR / MAP + p50/p95/p99 + QPS) with a committed SciFact baseline |
| 62 notes under `docs/notes/discovered/` | the negative-results corpus — the experience-track spine |
| 172 integration test files across 23 crates | the experiment register: every guarantee row in §5 must name one |

The system: **23 crates, ~101.5k lines of Rust, 172 integration test
files, 293 commits since 2026-05-24**, deployed on the DataZoo Hetzner
substrate as a Kamal stateful pet, with three consuming applications
(escurel-explore, herkules, peacock).

### The predecessor paper, and the numbers we inherit from it

`/home/jr/Projects/tmp/research/2026-05-16-skill-instance-paper/paper.md`
(2,368 lines, Nature register) *proposed* the skill–instance unification
and evaluated it on a ~430-line Python prototype. Its verification tree
is at `/home/jr/Projects/tmp/research/2026-05-15-kb-hypothesis-verification/`.
**Most of what this paper needs as a baseline already has a number.**
These are prototype-scale and must be relabelled as such, but they are
citable and they set the bar:

| source | result | how the new paper uses it |
|---|---|---|
| H-AGENT-1 (real LLM, 2026-05-17) | 100% task success, **12.4× cheaper than a flat-RAG baseline** | §6.2's prior. **A flat-RAG comparison already exists** — the new work is to reproduce it at five corpus scales on the real system |
| H-AGENT-2 (real LLM) | catalogue-first: 1 tool call; search-first, no preload: 2 calls; 8/8 correct | §6.2's tool-call row |
| H-MEMORY-1 | Tier-1 = **189 tokens invariant, 10²→10⁶ instances**; eager counterfactual 23.6 M tokens at 10⁶ (~800× a 30k budget) | §6.2's headline, relabelled as a *Tier-1* number |
| HYP-D2 | metadata-only reading 3.2–4.8k tokens @10k pages, 8–12k @100k; full-content 400–600k @10k | §2's cost model, Table 1 |
| HYP-E1 | **description-only top-1 83.3%** vs full-content 96.7% vs title-only 63.3%; Recall@10 96.7% | §6.4 — *the honest cost of the tier model*. Must be in the paper |
| H-SCALE-2 | typed backlinks **5.21 ms p95 at 100k instances/skill** (19× headroom) | §6.6 |
| H-SCHEMA-EVOLUTION-1 | 4/4 migration types validate cleanly | §5 guarantee row |
| H-SECURITY-1 | 16/16 attacks on executable instances handled, state preserved | §5 guarantee row — now *more* load-bearing, given `openapi`/`mcp` make outbound calls |
| H-AUTHORING-1 | **deferred** — needs human authors; engine path verified 8/8 error classes | §7. Still deferred; say so |
| LanceDB eval [4] | vector ties FAISS Flat at nDCG **0.975**; Lance hybrid 9.6 ms p50 vs DuckDB hybrid 20.5 ms; 551 KB vs ~3.5 MB on disk | §6.8, the ADR-0001 gate baseline |
| T3 10× scale [6] | vector holds **nDCG 0.933** @10,120 blocks; default-Tantivy FTS collapses to **0.350** | §6.9 negative result |
| T2 crash recovery [6] | audit-and-rebuild ~32 ms/page | §5 guarantee row E-5 |
| prototype e2e | 20 pages / 8 skills / 12 instances / 156 blocks / 47 links; **28/28 assertions**, H0–H9 + parser | §1 — one clause, as "the design was validated in miniature" |
| `references.md` (198 lines) | 13+ citations already formatted | seeds `refs.bib` |

**Positioning, to be stated explicitly in §1 and §8:**

> The design was proposed in [self-cite] and validated on a 20-page
> prototype. This paper reports what building it as a 101k-line system
> taught us, and measures the three claims the prototype could not: that
> the skill–instance duality bounds context cost independently of corpus
> size *on a real corpus*; that the same duality makes an instance's
> storage substrate a property of its type, so markdown, SQL, documents
> and live APIs share one referent space; and that the event–callback
> duality lets writes scale by fan-out while remaining replayable.

Do **not** re-derive the model at the predecessor's length. §3 gets ~1.5
pages, not the predecessor's ten.

### What does not exist yet, and blocks measurement

1. **No real-LLM harness in this repo.** The predecessor's lived in the
   research tree and is prototype-shaped. §6.2 needs one at scale. Top
   schedule risk.
2. **No 460-block / 10,120-block corpus in this repo.**
   `docs/eval/README.md` says so; ADR-0001's gate is still open because
   of it.
3. **No flat-RAG config in `escurel-eval`** (the *prototype's* flat-RAG
   comparison is not the shipped system's).
4. **No cascade/replay instrumentation** in the runner.
5. **No backend-matrix harness** exercising all five substrates through
   one tool sequence.

---

## The thesis

**Two dualities, plus one corollary, are what make agentic knowledge
management scale.**

**D1 — Skill ⟷ Instance (static; type ⟷ value).**
A skill is both a schema and a prompt; an instance is both a row and a
document. The consequence is mechanical, not stylistic: an agent's
*discovery* cost scales with the number of **skills** (human-authored,
O(10²)) rather than the number of **instances** (machine-generated,
O(10⁶)). Progressive disclosure bounds context cost *only because* typing
lets discovery happen at the type level. Without the type, "disclose
progressively" has nothing to disclose *about*.

**D1′ — the corollary: the type declares where the value lives.**
Because `backend:` is declared on the **skill**, not per instance, an
instance's *storage substrate* is a property of its type. One referent
space, five substrates: native markdown; a read-only SQL view over an
external relational source; an extracted document; a live REST/OpenAPI
call; a live upstream MCP call. The agent issues the same
`search → resolve → expand → neighbours` sequence against all of them and
never learns a second addressing scheme — no second tool surface, no
second identity model, no "is this a document or a row?" branch in the
prompt. `@version` on a link site pins an external snapshot
(`[[table::customers.churn@v14]]` → a DuckLake snapshot), so time-travel
into external data is also expressed in the one link syntax. This is the
part of the thesis that speaks directly to *knowledge management*: an
organisation's knowledge is not all markdown, and any model that requires
it to be is a model of a demo.

**D2 — Event ⟷ Callback (dynamic; log ⟷ projection).**
An event is an immutable fact; a callback is a projection of it into
state, and the projection's instructions are *themselves* a skill page.
Writes scale by fan-out across independent events rather than through a
central writer, and state is always re-derivable from the log — so a
crashed cascade replays rather than corrupting.

**The cross-product is the actual claim.** D1 supplies each callback with
both its instructions (the `label_skill` page *is* the agent's prompt) and
its acceptance test (the skill's frontmatter schema *is* what the write
must validate against). D1′ means the callback can fold an event into an
instance whose data is a Postgres row or a CRM record reached over REST,
not only into markdown — so the loop reaches systems escurel does not
own. D2 supplies D1 with content and keeps it from going stale. Neither
leg works alone: skills without events are a wiki that rots; events
without skills are an append-only log nobody can query; either without D1′
is a knowledge base that has to import the world before it can reason
about it.

**How each leg could be false, and where we test it:**

| leg | fails if… | test |
|---|---|---|
| D1 | skill count grows with instance count | §6.5, Figure 2 |
| D1 | typed retrieval is no better than flat RAG on the same bytes | §6.3–§6.4 |
| D1 | the context saving is illusory — the agent just expands more bodies | §6.2 reports Tier-1 *and* end-to-end tokens |
| D1′ | the agent's tool sequence differs per backend, or the abstraction leaks into the prompt | §6.7 — identical call shape across five substrates, measured |
| D1′ | live backends are too slow or too unreliable to sit behind `expand` | §6.7 — per-substrate latency + failure semantics |
| D2 | cascades do not quiesce | §6.6 |
| D2 | replay diverges | §6.6′ — kill-and-replay convergence |

---

## Style contract

Inherit `duckdb-gdrive/docs/paper/`'s register wholesale — it is already
calibrated against the two PVLDB reference demo papers:

- **Bold run-in paragraph headers** inside sections, not deep nesting.
- **Present tense, first-person plural, active voice.**
- **One idea per paragraph; the paragraph names itself.** ~8 lines max.
- **Every numeric claim states its configuration inline.**
- **Hedge honestly, then commit.** §7 is where this earns its keep.
- Related work is short and comparative, near the end.

Three escurel-specific additions:

- **No claim about agent behaviour without a real LLM run.** Stub-tokenizer
  numbers are labelled in the table, not only in the prose.
- **Prototype-era numbers are labelled prototype-era**, every time. The
  inherited table above is a *baseline*, not a result of this system.
- **State what is not implemented.** `docs/spec/README.md` marks the
  multi-tenant `TenantManager` as not yet implemented (one shared
  `Indexer` today). Any tenancy claim must say so.

---

## Section outline

### 1. Introduction (~1 page)

Four run-in-headed paragraphs plus contributions. **Drafted last.**

- **Agent memory is a context problem, not a storage problem.** Storing a
  million facts is trivial; getting the right ones into a bounded window,
  repeatedly, cheaply, is the difficulty. Flat vector memory answers
  "what is similar to this string" and nothing else.
- **Two dualities and a corollary.** D1, D1′, D2 in three sentences each.
  Assert the cross-product claim.
- **What we built.** 23 crates, 101k lines, one MCP surface over five
  storage substrates, one DuckDB file per tenant, and an external runner
  that holds all the automation so the gateway holds none.
- **What we measured, and what surprised us.** Forward-reference §6.2's
  headline and §6.9's sharpest negative result.

**Contributions.** (i) the two-duality model with its substrate corollary,
and the mechanism by which it bounds context cost; (ii) a design that
realises it — typed wikilinks validated at index time, one embedded store
for five concerns, five instance backends behind one referent space, an
event→callback loop whose reducer generalises from one-hop cascades to
fan-out workflows; (iii) a guarantee-by-guarantee verification protocol
where every row names a test that actually runs; (iv) an evaluation
against flat RAG on identical bytes at five corpus scales, plus a
per-substrate cost profile; (v) negative results.

Cite: Anthropic Agent Skills, DuckDB [SIGMOD'19], DuckLake, MemGPT/Letta,
mem0, Zep/Graphiti, GraphRAG, A-Mem, generative agents' memory stream,
event sourcing, and the predecessor paper. Seed `refs.bib` from the
predecessor's `references.md`.

### 2. Background: what flat memory costs (~1 page)

- **2.1 The tool-call cost model.** Per-task cost is *tokens in context* ×
  *turns*, both set by how much the KB makes the agent read before it can
  act. Define **Tier 1** (metadata) and **Tier 2** (bodies, via a
  deliberate `expand`).
- **2.2 Flat vector memory has no Tier 1.** With no types there is nothing
  to enumerate, so every question is a similarity query over everything
  and every answer is a body. **Table 1** goes here, not in §6 — the
  design is unintelligible without it. Rows from HYP-D2: metadata-only
  3.2–4.8k tokens @10k pages vs full-content 400–600k @10k.
- **2.3 Flat memory also has one substrate.** Everything must be imported
  and chunked before it can be reasoned about; a Postgres table or a CRM
  record enters as a stale copy or not at all. This paragraph motivates
  D1′ and is the one most reviewers will not have seen argued.
- **2.4 "Just use a bigger window" is not the answer.** Cost, latency,
  lost-in-the-middle. One cited paragraph, not belaboured.
- **2.5 What a write costs.** Flat memory's write path is "embed and
  append": nothing validates, supersedes, or links. State the four
  index-time checks as the contrast.
- **2.6 The staleness gap.** A typed KB with no ingestion loop is a wiki
  that rots. Motivates D2; rules out reading this as a data-model paper.

### 3. Two dualities and a corollary (~1.5 pages)

- **3.1 Skill ⟷ Instance.** An Agent-Skill page and a PKM note are the
  same artefact. `skill:` frontmatter, typed `[[skill::id]]` links with
  the type in a `link_skill` column, the four index-time checks. **The
  scaling argument in one paragraph:** discovery enumerates skills;
  skills are authored, so their count is bounded by human attention, not
  by ingestion rate.
- **3.2 The link syntax carries the type, the anchor and the version.**
  The six link forms; `#anchor` reaches a block, `@version` pins a
  snapshot, `|alias` handles presentation. Note the ambiguity this closed:
  in the untyped predecessor, `[[Anna Mueller]]` and `[[anna-mueller]]`
  were two nodes.
- **3.3 The type declares where the value lives (D1′).** The five
  backends, one sentence each, and the invariant that unifies them:
  **every external instance keeps a markdown overlay page**, so identity,
  links, ACL and history reuse the existing machinery and all novelty is
  confined to *where the body comes from*. Contrast materialised
  (`sql_view`, `document` — indexed, searchable, read-only) against live
  proxy (`openapi`, `mcp` — nothing in DuckDB, `search: "none"`, fetched
  per `expand`, write-back via `write_instance`). Worked example: a
  `dashboard` instance whose body is a list of `[[table::*]]` and
  `[[query::*]]` links beside prose.
- **3.4 Event ⟷ Callback.** An event is an instance of an event-typed
  skill with an `at:` field; the inbox is the unprojected suffix of the
  log; a callback is a harness run whose instructions are the event's
  `label_skill` page and whose toolset is `/mcp`. State the inversion:
  **state lives in the KB, not in a script's variables** — which is why a
  crashed run resumes.
- **3.5 The cross-product.** The four cells (schema validates the write;
  prompt drives it; event is provenance; callback is current value), plus
  D1′'s contribution: the callback can write *through* the KB into a
  system escurel does not own. Name the three degenerate corners.

**Figure 1** — the model on one page: skill/instance vertical, the
event→callback loop horizontal, the five substrates fanning out of the
skill's `backend:` block, numbered callouts ①–⑧ referenced from §3.
`docs/project-memory-infographic.svg` is a starting point, not the figure.

### 4. Design and implementation (~2 pages)

Attach each choice to the measurement or incident that produced it —
including the ones we got wrong first.

- **4.1 One file, five concerns.** Relational + HNSW (`vss`) + BM25
  (`fts`) + `crdt_ops` + attached DuckLake, with `pages/` canonical and
  everything else regenerable by audit-and-rebuild. Cite ADR-0001 and the
  Lance+DuckDB two-store baseline it replaced. Deliberate consequence:
  `git diff` still works on the corpus.
- **4.2 Typing at index time.** The wikilink parser is a regex, not a
  markdown AST — the AST fragments on `[`; cite the evaluation. The four
  checks, and what happens to a link that fails one.
- **4.3 One referent space, five substrates.** The implementation of D1′,
  and the section a security reviewer will read first:
  - **Secrets never enter markdown.** `sql_view` names a credential
    registered out-of-band (`register_credential`, realised as a DuckDB
    `CREATE SECRET`); `list_credentials` returns names only.
  - **No SSRF surface.** A remote skill names an **admin-registered
    endpoint**, never a raw URL, so tenant markdown can never make the
    server fetch an arbitrary host. A `kind` mismatch between skill and
    registered endpoint fails closed.
  - **Never fabricate a body.** A live read that times out returns the
    overlay page plus `backend_projection.issue` — the `binding_degraded`
    policy — never a partial body.
  - **Writes are value-bound.** Remote write-back templates are filled
    from scalar payload fields, never string-spliced; an unresolved
    placeholder fails the call rather than sending a literal `{x}`.
  - **Schema drift is detected, not tolerated.** `sql_view` captures a
    `source_schema_fingerprint` at creation and revalidates on attach.
- **4.4 The write path.** Loro CRDT op stream for live editing;
  `update_page` whole-page fallback; per-tenant write lock; the DuckDB
  transaction as atomicity primitive; markdown written write-then-rename
  *after* commit. Non-markdown backends are `writable: false` on this
  path by construction — remote write-back is the separate, explicitly
  named `write_instance` tool.
- **4.5 The runner: all automation outside the gateway.** Webhook + inbox
  poller converging on one bounded queue; a runner-local durable ledger
  (never the tenant store); idempotency, dedup, depth/budget caps, cycle
  prevention; the `Harness` trait over Claude Code / Codex / ADK / echo.
  Say why the gateway stays automation-free — it is what keeps the KB a KB.
- **4.6 One loop, two settings.** The cascade emitter is a reducer with
  width ≤ 1 and no join; a workflow is the same reducer with an explicit
  plan, fan-out N and a quorum barrier. The paper's strongest engineering
  claim after D1: the fan-out orchestration everyone is building is not a
  second execution model.
- **4.7 Distributing types: page layers and skill packs.** A pack is a
  shippable *type library* — read-only `base@<pack>@vN` pages, overlay
  shadowing, curator-gated promotion, export/import/rebase. With D1′ a
  pack ships not just a schema but a *binding* — a `crm` pack carries the
  skills whose instances resolve against the subscriber's own CRM
  endpoint. This is the paragraph that generalises beyond one deployment.
- **4.8 What we deliberately refuse.** No raw SQL on the agent surface, no
  raw vector access, no cross-tenant operations, no auto-provision, no
  raw URLs in tenant markdown. One clause of justification each.

### 5. Guarantees and their verification (~1.5 pages)

Placed **before** the evaluation, as in the gdrive paper: the evaluation
is only interesting once the reader knows what is promised.

**Table 2.** One row per promise; columns for what escurel guarantees,
what a flat vector store guarantees, and the experiment id. Every
`\expref{n}` must name a test that actually runs in
`cargo test --workspace --all-targets` — a claim whose test is `#[ignore]`d
is not a claim, and this repo has a standing rule about exactly that
failure mode.

| # | promise |
|---|---|
| E-1 | `resolve` of a valid `[[skill::id]]` is deterministic and total |
| E-2 | a link failing any of the four index-time checks is reported, never silently dropped |
| E-3 | `[[skill::id@version]]` never resolves to different bytes across time (markdown: canonical; external: the pinned snapshot) |
| E-4 | concurrent CRDT sessions on one page converge |
| E-5 | mid-write `SIGKILL` leaves markdown ⟂ DuckDB reconcilable by audit; rebuild recovers from markdown alone (prototype: ~32 ms/page) |
| E-6 | a token for tenant A cannot read tenant B — **and what is actually enforced today**, given `TenantManager` is unimplemented |
| E-7 | a cascade terminates: depth cap, cycle detector, budget |
| E-8 | replaying a run's event log reproduces its instance state |
| E-9 | a pack's pinned version never moves silently; promotion passes the scrub gate |
| E-10 | every non-markdown backend is read-only on `update_page` / `apply_op` (`backend_read_only`) |
| E-11 | no tenant-authored text can direct an outbound request at an unregistered host |
| E-12 | a credential or endpoint secret is never echoed by any read tool |
| E-13 | a failed live read yields `binding_degraded`, never a partial or fabricated body |
| E-14 | a remote write with an unresolvable placeholder fails closed |
| E-15 | source schema drift is detected via the fingerprint, not silently projected |
| E-16 | a forward-compatible skill migration validates existing instances (prototype: 4/4 types) |

Where the gdrive paper compared Drive against S3, escurel's comparator is
**flat vector memory** and, where a promise has no analogue there, **a
document database**. Rows with no honest analogue say "n/a — the system
has no such notion", which is itself the finding.

### 6. Evaluation (~2.5–3 pages)

**6.1 Setup and protocol.** Machine, DuckDB version, embedder
(`BAAI/bge-base-en-v1.5`, 768-d BERT — state the two constraints that pin
it: `blocks.dense_vec` is `FLOAT[768]` and `CandleEmbedder` loads BERT
only), reranker, LLM + version + temperature for agent-behaviour runs, and
the rule that compared legs are measured in one session.

Every experiment is a `\todonum{}` placeholder in `paper.tex` until its
harness runs.

---

#### 6.2 Context cost against corpus size — the headline (D1)

**Table 3.** For |instances| ∈ {10², 10³, 10⁴, 10⁵, 10⁶}, three arms:

| | escurel | flat RAG | whole corpus in context |
|---|---|---|---|
| Tier-1 tokens (cold start) | | n/a | |
| tokens to first correct answer, median | | | |
| tool calls to answer, median | | | |
| end-to-end tokens, 10-task session | | | |
| task success rate | | | |

- **Prior to beat.** The prototype's real-LLM run reported 100% success at
  **12.4× cheaper than flat RAG**, and 1–2 tool calls to answer. The new
  contribution is the *scale sweep* and a real system; cite the prior
  explicitly as prototype-era.
- **Metric definitions fixed in advance.** "Correct" is graded against a
  pre-registered answer key by exact match on a named entity or numeric
  field — not an LLM judge. Medians of n = 10.
- **Configuration.** Identical corpus bytes across arms. Flat RAG = the
  same markdown, chunked at the same block boundaries, same embedder,
  top-k retrieval, no types, no filter, no wikilinks. Report the top-k
  tuning; an untuned baseline is a straw man.
- **Honest scope.** Tier-1 cost is invariant; *total* context still grows
  with how many bodies the agent expands. Report both. The 189-token
  invariant is a Tier-1 number and must be labelled as one.
- **Harness (new, H-3, ~2 d).** `escurel-eval --mode context-cost`.

#### 6.3 Retrieval quality against flat RAG on identical bytes (D1)

**Table 4.** nDCG@10, recall@100, MRR@10, p50/p95, QPS for
`flat` · `single_pass` · `two_pass` · `rerank` · `two_pass_rerank`, on
SciFact (committed baseline) and on escurel's own labelled corpus once it
is in BEIR format. Four of five configs run today; `flat` is new (H-1).

#### 6.4 Ablation: is it the type, or just a filter? (D1)

**Table 5.** 2×2 over {skill filter on/off} × {typed expansion on/off} on
the §6.3 queries. **Include the predecessor's HYP-E1 result as the
counterweight**: description-only indexing reached top-1 83.3% against a
full-content baseline of 96.7%. The tier model *costs* retrieval accuracy
at the discovery step and buys it back in tokens; a paper that reports
only the token win is selling something.

#### 6.5 Does the skill count stay bounded? — the D1 falsifier

**Figure 2.** Skills (y₁) and instances (y₂) against time over the real
corpus history of the three consuming tenants. If skills grow linearly
with instances, D1 is false and the paper says so. Report the ratio, and
state the regime: 10 weeks of one organisation's data, not a scaling law.

#### 6.6 The event→callback loop, and replay (D2)

**Table 6.** Over a seeded stream of N events across k skills:
events/second end-to-end, agent runs per event, cascade depth
distribution (p50/p95/max), time-to-quiescence, and the fraction of
cascades stopped by each loop control (depth cap, budget, cycle detector,
dedup). Measured with the `echo` harness so the number measures *the
loop*; one run with a real harness for the constant factor. Same table at
workflow fan-out width N ∈ {1, 4, 16} with a quorum barrier, showing the
§4.6 generalisation costs what the width says and nothing structural.

**Replay (E-8).** `SIGKILL` the runner at k = 50 pseudo-random points in
a cascade of known shape; restart; replay; assert final instance state is
byte-identical to the uninterrupted run. Report convergence rate,
divergence classes, and mean replay time as a fraction of the original.
*One divergence class found and fixed is a better result than 50/50 clean.*

#### 6.7 Five substrates, one call shape (D1′)

The experiment that makes the corollary a result rather than an assertion.

**Table 7.** For each backend ∈ {markdown, sql\_view, document, openapi,
mcp}, the same agent task executed against an instance of that backend:

| | markdown | sql_view | document | openapi | mcp |
|---|---|---|---|---|---|
| tool calls to answer (identical sequence?) | | | | | |
| tokens to answer | | | | | |
| `expand` p50 / p95 (ms) | | | | | |
| indexed / searchable | yes | yes | yes | no | no |
| writable, and by which tool | `update_page` | — | — | `write_instance` | `write_instance` |
| behaviour when the source is down | n/a | | | | |

- **The claim under test** is that row 1 is *identical across all five
  columns* — same tools, same order, no backend-specific branch. If it is
  not, D1′ leaks and the paper reports where.
- **The second claim** is the latency spread: materialised backends serve
  from DuckDB, live proxies pay a network round trip inside `expand`. That
  spread is the honest cost of D1′ and belongs in §7.
- **Failure semantics** are measured, not asserted: kill each external
  source mid-task and confirm `binding_degraded` (E-13) rather than a
  fabricated body.
- **Harness (new, H-9, ~1.5 d).** One fixture tenant with five skills, one
  per backend, over the same logical entity (a customer), against a real
  Postgres, a real uploaded PDF, a real REST stub and a real upstream MCP
  server — no mocks, per this repo's merge gate.

#### 6.8 What the single-file collapse cost (optional, ~0.5 d)

**Table 8.** ADR-0001's pre-deployment gate — declared in advance in
2026-05 and **still open**: nDCG at 460 and 10,120 blocks, p50 vector
latency, p95 vector+filter at 100k instances, against the Lance baselines
(0.975 / 0.933 / 4.3 ms / 18.71 ms; Lance hybrid 9.6 ms p50 vs DuckDB
hybrid 20.5 ms; 551 KB vs ~3.5 MB on disk).

Include **only if the corpus is recoverable**. Closing a gate the project
declared in advance is exactly the credibility an experience paper trades
on. If the corpus is gone, say so in §7 rather than quietly dropping it.

#### 6.9 Negative results

Two, from the 62-note corpus and the inherited results. Strongest:

1. **FTS ranked synonym mutants above originals** — nDCG 0.350 at 10,120
   blocks under default tokenizer settings, against 0.933 for vector. The
   natural thing to believe was that lexical search degrades gracefully.
2. **The default embedder cannot be loaded.** The spec pins
   EmbeddingGemma; `candle-transformers` has no gemma3 sentence-encoder
   path, so the real default is a BERT model. A locked decision the
   ecosystem quietly invalidated.

Runners-up: a tenant-isolation hole tests did not catch
(`2026-07-08-cross-tenant-token-not-enforced.md`); `frontmatter_index` as
a designed index nothing ever queried.

### 7. When you should not use this (~0.5 page)

- Corpora with no natural types — if everything is one skill, D1 buys
  nothing and you have paid for a schema you do not have.
- Write rates that outrun a per-tenant single writer.
- Sub-millisecond retrieval budgets: an embedded HNSW in a shared file is
  not a dedicated vector service.
- **Live-backend latency.** An `expand` that proxies a REST call inherits
  that call's tail. If your source is slow or flaky, materialise it.
- **Live backends are not searchable.** `search: "none"` is a real
  limitation, not a footnote: an agent cannot find what it cannot index,
  so remote instances must be reachable by link or by overlay metadata.
- Teams unwilling to author skills — the type system is human-authored by
  construction; that is the cost side of D1.
- Anything needing cross-tenant federation today.

### 8. Related work and summary (~0.5 page)

Short and comparative. Position against: Agent Skills (procedural axis
only, no episodic typing), MemGPT/Letta (paging, no type system), mem0 and
Zep/Graphiti (machine-*inferred* schema — the sharpest contrast with
authored skills), GraphRAG (index-time summarisation, no write loop),
A-Mem, generative agents' memory stream, PKM tools (typing but no agent
surface and no event loop), federated/virtual query engines and lakehouse
catalogs (the D1′ comparator: they unify *data* access without unifying
*knowledge* addressing), and event sourcing as D2's ancestor. Then the
predecessor paper, positioned per §Context.

Close on the cross-product claim and what would falsify it at a scale we
could not reach. One paragraph — no more — pointing at the compile-first
wiki (`distill` / `lint` / `freshness` / `eval-improve`) as future work.

---

## Harness work register

| # | work | est. | unblocks |
|---|---|---|---|
| H-1 | `escurel-eval` `flat` config — same bytes, no typing, no filter | 1 d | §6.3, §6.4 |
| H-2 | escurel's labelled corpus in BEIR format (locate in research tree or rebuild) | 0.5–2 d | §6.3, §6.8 |
| H-3 | `escurel-eval --mode context-cost` + real-LLM driver + pre-registered task set | 2 d | §6.2 — **the headline** |
| H-4 | runner cascade instrumentation (depth, quiescence, control-hit counters) | 1 d | §6.6 |
| H-5 | kill-and-replay harness over the deterministic step key | 1 d | §6.6 |
| H-6 | skill-vs-instance growth extraction across the three tenants | 2 h | §6.5 |
| H-7 | ADR-0001 gate run (needs H-2) | 0.5 d | §6.8 |
| H-8 | figure generation: `plot_*.py` → pgfplots, mirroring gdrive's measure/render split | 0.5 d | Figs 1–3 |
| H-9 | five-backend fixture tenant + matrix harness (real PG, real PDF, real REST stub, real upstream MCP) | 1.5 d | §6.7 |

Total ≈ 8.5–11 days. H-3 is the schedule risk and the centre of gravity;
start it first, in parallel with prose. H-9 is the second-largest and the
one that makes D1′ a result.

**Rule inherited from gdrive:** re-measuring and re-rendering are separate
`make` targets. `make figures` regenerates TeX from committed JSON and is
offline and deterministic; `make measure_*` needs live credentials.

---

## Rules for this manuscript

1. **No number appears that a harness cannot reproduce.** Unmeasured is
   `\todonum{...}` and renders red, so it cannot ship unnoticed.
2. **Every guarantee row cites an experiment id** mapping to a test that
   actually runs in `cargo test --workspace --all-targets`.
3. **Cross-session ratios are forbidden.** Compared legs are measured in
   one session, one model cache, one machine.
4. **No agent-behaviour claim without a real LLM run.**
5. **Prototype-era numbers are labelled prototype-era, every time.**
6. **State what is not implemented** — `TenantManager`, the unloadable
   default embedder, the open ADR-0001 gate, H-AUTHORING-1's deferral.
   An experience paper that reports only the parts that worked is an
   advertisement.
7. **Negative results stay in.**

---

## Build setup

Mirror `duckdb-gdrive/docs/paper/`:

```
docs/paper/
  plan.md            # this file
  paper.tex          # acmart sigconf, nonacm
  refs.bib           # seeded from the predecessor's references.md
  cls/               # vendored acmart + the pifont no-op stub
  Makefile           # make -> paper.pdf, prints page count against the 8-10 budget
  data/*.json        # committed harness output
  fig_*.tex          # generated from data/ by `make figures`
  .gitignore
```

Copy unmodified: the `\todonum` and `\expref` macros, the `\ding` no-op
that keeps the build off `texlive-fontsextra`, the `pages` target, and the
measure/render split.

---

## Delivery sequence

One logical change per PR, per `CLAUDE.md` principle 6.

| PR | content | gate |
|---|---|---|
| 1 | scaffold `docs/paper/` — Makefile, vendored `cls/`, `refs.bib` from the predecessor, `paper.tex` with all eight sections as outlined stubs carrying this plan's notes as comments | `make` builds; 0 numbers; page count printed |
| 2 | §2 Background + §3 dualities + Figure 1 | prose only; every claim cites an existing doc |
| 3 | H-1 + H-2 + H-3; commit `data/*.json` | harnesses run from a clean checkout |
| 4 | §4 Design and implementation | every subsection names the ADR or note behind it |
| 5 | §5 Guarantees; wire every `\expref` to a real test name | grep every cited test name against the tree |
| 6 | H-9 + §6.7 (five substrates) | no mocks at the backend boundary |
| 7 | H-4 + H-5; §6 prose + Figures 2–3 | no `\todonum` in a section marked drafted |
| 8 | §1 (written last), §7, §8; `\balance`; page trim; codex review per principle 9 | 8–10 pages; zero red |

At each merge: remove the worktree, run `scripts/reclaim-disk.sh --all`.

---

## Open risks

1. **H-3 is the paper.** Without the real-LLM context-cost sweep, §6.2 is
   a stub and the central claim is unmeasured. Build it first; pre-register
   the task set and answer key before the first run.
2. **The corpus may be unrecoverable.** §6.3 and §6.8 both want escurel's
   labelled corpus. SciFact substitutes for §6.3 alone; §6.8 has none.
3. **§6.7 needs four real external sources** — a Postgres, a PDF, a REST
   stub, an upstream MCP server — under this repo's no-mock merge gate.
   Underestimating this is how §6.7 becomes an assertion again.
4. **Reviewers will attack the flat-RAG baseline as a straw man.** Defend
   it by construction in §6.1: identical bytes, identical chunk
   boundaries, identical embedder, tuned top-k, tuning reported.
5. **Predecessor overlap.** If §3 reads as restatement, the paper has no
   contribution. Cap §3 at 1.5 pages; let §4, §6.7 and §6.2 carry it.
6. **Page budget.** Eight sections, eight tables and three figures is very
   tight at 10 pages. §3 and §8 are the compressible ones; §5, §6.2 and
   §6.7 are not. If something must go, §6.8 goes first — but say so.
7. **Scope creep into the compile-first wiki.** One paragraph in §8, no
   more. It is a second paper.
