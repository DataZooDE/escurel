# Complexity reduction plan

**Date:** 2026-08-05. Sources: two independent analyses (Claude Fable 5,
OpenAI Codex) plus direct verification of every load-bearing claim below.
Where the two disagreed, the disagreement is recorded rather than averaged.

## The premise, corrected

The working hypothesis was "the codebase is large for the functionality."
Measured:

| | LOC |
|---|---|
| `crates/*/src` | 57,210 |
| `crates/*/tests` | 45,166 |
| ratio | 0.79 |

So the often-quoted ~102k is **56% source, 44% tests**. Under a no-mock
integration-test policy a 0.79 ratio is healthy, and none of what follows
proposes touching tests. The question is whether **57.2k of source** is large
for 66 MCP tools, five instance backends, DuckDB (relational + HNSW + BM25 +
CRDT), skill packs, page layers and an event→agent projection runner.

Both analyses land in the same place: **65–75% is irreducible feature
surface, 10–15% is comments, and 15–25% is accidental complexity** —
concentrated almost entirely in one file. The codebase is not carrying a
redundancy epidemic. It is carrying one 7,086-line god-file and one
abstraction that was designed and then bypassed.

## What is *not* the problem

Stated explicitly, because two obvious candidates turned out to be fine.

**The trait boundaries earn their keep.** `LaneStore` has four
implementations (fs/s3/gcs/duckvfs), `Embedder` five, `Harness` four
(echo/codex/claude/adk). No single-implementation trait is adding indirection
for its own sake — the usual Rust over-abstraction failure is absent.

**`mcp.rs` contains zero SQL.** It delegates to the indexer (39 call sites)
and holds no persistence logic. Its size is glue and enumeration, not a
smuggled data layer.

**`config.rs` and `indexer.rs` are wide but cohesive.** `config.rs` is mostly
struct definitions plus one builder; `indexer.rs`'s methods are all genuinely
`Indexer` behaviour. Neither is a god object, and neither is on this plan's
critical path.

## Where I disagree with one of the analyses

Codex proposed, as its second-highest-value item, *"move long protocol/config
docs out of code — save 800–1,200 source lines."* **Rejected.** Comments are
not complexity; they are the mitigation for it. `mcp.rs` carries 1,029 comment
lines and `config.rs` opens with an 80-line environment-variable table, and
both are why anyone can navigate those files at all. Deleting them would
reduce LOC while *increasing* the cost of every future change. This repository
already treats documentation drift as a first-class defect; optimising the
line count against that is optimising the wrong metric. The line total is a
symptom to be explained, not a target to be hit.

## The plan

Ordered by (value × certainty) ÷ risk. Line estimates are the analyses'
projections, not measurements — treat them as order-of-magnitude.

### R1 — Split `mcp.rs` into modules by concern *(first, and risk-free)*

7,086 lines currently mixing: JSON-RPC framing (`:151`), ingest REST handling
(`:334`), reader-mode gating (`:1127`), tool dispatch (`:1293`), ~50 business
handlers (`:1441`+), schema generation (`:5806`), OpenAPI generation
(`:6811`).

Split into `mcp/{dispatch,tools_read,tools_write,tools_admin,wire,schema}.rs`.
**Removes zero lines.** Do it first anyway: it is a pure file-move with `mod`
re-exports, it converts one unreviewable file into six reviewable ones, and
every item below becomes a separately auditable change instead of another
edit to the same 7k-line blob.

*Risk: very low. Breaks nothing if re-exports are complete.*

### R2 — One source of truth for the tool registry *(highest value)*

Each of the ~66 tools is currently declared in **three** unlinked places:

| registry | location |
|---|---|
| dispatch arm | `mcp.rs:1293` |
| discovery schema | `mcp.rs:5806` `tools_list_payload()` |
| execution label | `mcp.rs:6754` `DETERMINISTIC_TOOLS` |

Verified: for `list_skills`, `expand`, `capture_event`, `update_page`,
`register_credential`, each name appears once in dispatch and twice more,
with nothing forcing the three to agree.

This is the most valuable item in the plan and the line saving (300–500) is
the least interesting thing about it. **It removes a bug class**: a tool can
be dispatchable but undiscoverable, or discoverable with a stale schema, or
mislabelled `deterministic` when it orchestrates — and nothing fails to
compile. That is the same failure mode this repository already documents for
consumer-skill drift, reproduced inside one file.

Fix: a declarative registry (const table or macro) from which dispatch,
schema and labels are all derived, so adding a tool is one entry.

*Risk: medium. Touches discovery, admin gating and execution labels. The
`cli_parity` test already ratchets tool→CLI coverage and will catch omissions.*

### R3 — Route the four external backends through `InstanceBackend`

Verified: `grep -rn "impl .*InstanceBackend for"` returns **exactly one hit**
(`backend/markdown.rs:33`). The other four backends — `sql_view`, `document`,
`openapi`, `mcp` — bypass the trait entirely and are special-cased by
`backend_ref.kind` in the presentation layer (`mcp.rs:1726, 1757, 1812,
1877`).

The two analyses disagreed here. Fable called it a missing abstraction with
duplication filling the gap; Codex called the trait "borderline but
increasingly justified" and warned against removing it. Both are right about
different things, and the resolution is the same either way: **do not delete
the trait — route the others through it.**

Worth doing even at zero line saving, because it is an open/closed violation
sitting exactly where the architecture claims to be extensible. Today each new
backend costs another branch in the dispatcher; afterwards it costs one trait
impl. (This also falsifies a claim the now-deleted paper made — that adding a
substrate required no dispatcher change. It required four.)

*Estimated saving: 150–250 lines. Risk: medium — touches the read path for
three of five backends. The backend integration tests cover it.*

### R4 — Collapse the parse → call → wrap boilerplate

Measured in `mcp.rs`:

| pattern | count |
|---|---|
| `serde_json::from_value(args)` | 56 |
| `JsonRpcError::internal(format!` | 135 |
| `invalid_params(format!` | 86 |
| one-off `XArgs` structs | 48 |

A five-line skeleton repeated ~50 times. A generic
`parse_args<T>(args, tool) -> Result<T, JsonRpcError>` plus a small macro for
"call indexer, map error with tool name, wrap in `json!`" reduces each handler
to its actual content.

*Estimated saving: 1,000–1,500 lines — the largest in the plan. Risk: low;
mechanical, and each handler already has a test. Watch for error-message text
that tests assert on.*

### R5 — Shared request context for MCP and ingest

Token/group extraction in `mcp_inner` (`mcp.rs:238`) mirrors `ingest_gate`
(`mcp.rs:391`); auth/quota/indexer capture repeats at `:162`, `:184`, `:371`.
Extract one caller-context builder.

*Estimated saving: 80–120 lines. Risk: medium — auth edges are exactly where
a missed case matters; this is the item with the worst failure mode relative
to its size.*

### R6 — Page-materialisation helper

Parse frontmatter → derive skill/slug/at/scenario → delete+insert `pages` →
delete+insert `blocks` appears twice: `indexer.rs:698–888` (markdown) and
`indexer.rs:1041–1122` (document overlays).

*Estimated saving: 120–180 lines. Risk: medium — document chunk indexing and
markdown rebuild semantics both depend on it.*

### R7 — Append-surface routing helper

The `OnceLock` + `*_backend` + `has_shared_*` + `attach_*` shape repeats for
chat, events and CRDT (`indexer.rs:322, 375, 483`), and the reader gate
repeats three times (`mcp.rs:1127, 1140, 1153`).

*Estimated saving: 150–250 lines. Risk: medium — DuckLake reader/writer
routing.*

### R8 — Object-store layout helper

`object_key` and `list_prefix` are near-identical in `s3.rs:139` and
`gcs.rs:127`.

*Estimated saving: 40–70 lines. Risk: low. Smallest item; do it only when
already in that file.*

## Totals and honest expectations

| | |
|---|---|
| estimated removable | ~1,900–2,900 lines |
| as a share of `src` | **3.3–5%** |
| files structurally improved | `mcp.rs` (one file → six), backend dispatch |

**The line total will not move much, and that is the correct outcome.** The
hypothesis that prompted this — that the codebase is large for its
functionality — is true of `mcp.rs`'s glue and false of everything else. The
algorithmic core (index, CRDT, runner, storage) is appropriately sized for
what it does.

The reason to do R1–R3 is not the 3–5%. It is that two bug classes currently
have no compile-time guard: **a tool can drift out of sync with its own
schema, and a backend can be added without the abstraction that exists to
receive it.** Both are the kind of defect that is found in production rather
than in review, and both become impossible rather than merely unlikely.

## Suggested sequencing

1. **R1** (split) — unblocks reviewable work on everything else.
2. **R2** (registry) — highest value; removes the drift class.
3. **R4** (boilerplate) — largest saving; mechanical.
4. **R3** (backend trait) — fixes the open/closed violation.
5. **R6, R7, R5, R8** — as the surrounding code is touched for other reasons.

R1 through R4 are one focused piece of work. R5 through R8 are not worth a
dedicated project and should ride along with feature work in those files.

---

## Status (2026-08-07)

Recorded after the fact, because this document was written on a branch and
the work merged without it — for two days eight files in `main` cited a path
that did not exist there. The plan is the reason those modules are shaped the
way they are, so it belongs next to them.

| item | status |
|---|---|
| R1 — split `mcp.rs` | **done** (#346). 7,247 → ~1,339 lines plus six modules: `schema`, `ingest`, `tools_admin`, `tools_read`, `tools_write`, `backend_view` |
| R2 — one tool registry | **not done.** Guarded, not fixed — `tool_registry_conformance.rs` (5 tests) now pins the three registries against each other in both directions |
| R3 — route backends through a trait | **done differently** — see below |
| R4 — collapse parse/call/wrap | **done** (#346), and the estimate was wrong: projected 1,000–1,500 lines, actual **57** |
| R5–R8 | **not done**, as intended — they ride along with feature work in those files |

### R3 did not happen as written, and should not have

The prescribed fix — routing the four external backends through
`escurel_index::backend::InstanceBackend` — is unimplementable as specified.
That trait's `expand` returns an `ExpandedPage`, a domain value with nowhere
to carry `backend_projection`, `chunks_total` or a bounded block list. Those
are *presentation* concerns: they shape the JSON an agent receives, not what
the store holds. Following the plan would have pushed wire-shaping into the
storage crate to satisfy a trait — worse than the arrangement it replaced.

The duplication the item identified was real. It was fixed at the
presentation layer instead, as `mcp/backend_view.rs`, which does what the
item actually wanted: one place that knows the set of backend kinds, one
dispatch point per read tool.

### On R2, which is the one that matters

R2 was the highest-value item and it is still open. The conformance tests
make the drift *detectable*; only the unified registry makes it impossible.

That distinction stopped being theoretical during this work: the original
conformance test checked advertised → dispatchable only, and a `purge_page`
tool merged from `main` kept its dispatch arm while losing its schema entry —
callable, invisible to discovery, and all four tests passed. The fifth test
(`every_gated_tool_name_is_advertised`) closes that direction. A one-way
conformance check is half a guard.

### What the estimates were worth

R4 projected 1,000–1,500 lines and removed 57. Treat the "~1,900–2,900 lines"
total as what it was labelled — the analyses' projection, not a measurement —
and note that the document's own conclusion still held: the line count was
never the point, and the codebase is not large for its functionality outside
`mcp.rs`'s glue.
