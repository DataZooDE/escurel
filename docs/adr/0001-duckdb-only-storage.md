# ADR-0001 — DuckDB-only per-tenant storage

**Status:** Accepted, 2026-05-19. Pre-deployment retrieval-quality
gate (§Pre-deployment gate) is open and must pass before any
production rollout.

## Context

The v1 implementation needs to pick a per-tenant storage shape
for the four concerns the spec layer must serve: relational
metadata + the typed link graph, block-level retrieval (vector +
lexical), CRDT op persistence, and the canonical markdown
source-of-truth. An earlier prototype split this across two
stores (a LanceDB dataset for retrieval and a DuckDB file for
relational + link-graph state) plus a `crdt/<page_id>/` sidecar
directory for the Loro op log. This ADR records the decision to
collapse the per-tenant storage into a single DuckDB file
alongside the canonical `pages/` markdown directory.

The typed-page model the spec rests on is unchanged by this
decision: skill-instance markdown, typed `[[skill::id]]`
wikilinks, the four index-time validation checks, the twelve
agent tools, and the mandatory `escurel` meta-skill all carry forward
unmodified.

## Decision

Per-tenant storage is one DuckDB file (`escurel.duckdb`) alongside
the canonical `pages/` markdown directory:

```
tenants/<tenant>/
  pages/              # canonical markdown — source of truth
  escurel.duckdb      # pages, links, blocks (body + dense_vec + FTS),
                      # crdt_ops, frontmatter_index, ACL
  external.ducklake/  # ATTACH point for the origin axis (unchanged)
```

DuckDB owns four concerns:

1. **Relational metadata** — `pages` and `links` tables, with
   the `link_skill` column on `links` and the composite
   `(skill, at_ts)` index on `pages` (event-log scan support).
2. **Retrieval** — a `blocks` table with one row per markdown
   block carrying `(page_id, anchor, body, dense_vec, skill,
   page_type, at_ts)`; the `dense_vec` column is indexed by
   HNSW via the `vss` extension; the `body` column is indexed
   by BM25 via the `fts` extension. Denormalised
   `(skill, page_type, at_ts)` columns mean a filtered vector
   query is a single SQL statement.
3. **CRDT persistence** — a `crdt_ops` table with one row per
   applied Loro op `(page_id, op_id, hlc, parent_op_id,
   op_bytes)` plus a `crdt_snapshots` table for periodic
   `export_snapshot` blobs. The in-memory Loro engine is
   unchanged; only the persistence layer moves.
4. **Origin axis** — DuckLake catalogs ATTACH-ed read-only.
   Already DuckDB-native; no change required.

The `pages/` directory remains the canonical source of truth.
Everything in `escurel.duckdb` is derivable from `pages/` via
audit-and-rebuild. Git, tar, rsync, and Obsidian/IDE authoring
against `pages/` all continue to work; the `.duckdb` file is
regenerable and does not need to be checked in.

## Decision drivers

Three pieces of evidence prompted the decision.

1. **What the architecture actually buys at our scale.**
   Typed-backlink latency clears 5.21 ms p95 at 100 k
   instances — 19× below the 100 ms target. The headline
   retrieval-latency gap between the two-store path and the
   DuckDB-hybrid path was a few-millisecond p50 difference.
   Sub-100 k-block tenants (which is every tenant currently
   projected) are deeply in the regime where neither path is
   close to budget. The 2× absolute latency gap, in
   milliseconds, matters less than the architectural complexity
   it buys.
2. **Operational mass the two-store split carried.**
   Cross-store queries went through an Arrow bridge that added
   ~4 ms p50 and required serialising results in one engine and
   registering them as a temporary table in the other. CRDT
   persistence used a separate sidecar directory with its own
   atomicity reasoning. The audit function was a three-way set
   diff with three failure modes. The Rust spec carried two
   storage crates, two SIGKILL-atomicity stories, and a small
   Arrow-handoff codepath. Removing one storage engine removes
   all of that.
3. **Maturity of DuckDB's VSS and FTS extensions.** VSS now
   ships persistent HNSW indexes; FTS has had a year of
   attention. An earlier evaluation that recommended away from
   DuckDB ran on an earlier extension state and deserves
   re-evaluation under current versions — which is the
   pre-deployment gate below.

## Considered alternatives

- **(a) Keep two stores (status quo).** Lance for retrieval +
  DuckDB for relational + sidecar files for CRDT. Rejected for
  the operational-mass reasons in driver 2 and because the
  latency advantage that originally motivated Lance does not
  matter at our projected scale (driver 1).

- **(b) Collapse markdown storage into DuckDB and expose
  `pages/` via a FUSE virtual filesystem.** Considered briefly:
  it preserves a file API for tooling but breaks `git diff` on
  the corpus, complicates cross-platform deploy (FUSE is
  Linux-first, macFUSE on macOS, nothing native on Windows),
  and weakens audit-and-rebuild by removing the canonical
  file-on-disk to compare against. Rejected: keep `pages/` on
  disk as the canonical source; move only the index lanes and
  CRDT persistence into DuckDB.

## Consequences

### What this supersedes

| Item | Before | After |
|---|---|---|
| Per-tenant storage shape | per-tenant DIR with Lance + DuckDB; cross-store via Arrow | per-tenant DIR with DuckDB only; all queries SQL-native |
| `retrieval.lance/` dataset | LanceDB blocks + embeddings + FTS | DuckDB `blocks` table with `vss` HNSW index + `fts` BM25 index |
| `crdt/<page_id>/ops.log` + `snapshot.bin` sidecars | append-only files per open page | `crdt_ops` and `crdt_snapshots` tables in DuckDB; ops flush is a write transaction |
| Cross-store query path | DuckDB-attached on Lance Arrow | single SQL statement; vector search returns `(block_id, distance)` rows joinable directly against `pages`/`links` |
| Lance `.add()` torn-write argument | torn-write avoided by Lance MVCC | torn-write avoided by DuckDB transaction; mid-write SIGKILL rolls back |
| `audit` three-way set diff | markdown ⟂ Lance ⟂ DuckDB | markdown ⟂ DuckDB (two-way) |
| `lane.lance.retain_versions` config knob | Lance MVCC version retention | drop; DuckDB checkpoint policy replaces |

### What does NOT change

- The page-type vocabulary `{skill, instance}`.
- Typed `[[skill::id]]` wikilink syntax and the four index-time
  validation checks.
- The `link_skill` column on `links` and the typed-backlink
  query path. These were always DuckDB-native.
- The `pages/` directory as canonical source of truth.
- Audit-and-rebuild as the recovery model. The audit becomes a
  two-way diff (markdown ⟂ DuckDB) instead of three-way;
  rebuild semantics unchanged.
- The 12-tool MCP surface (`search`, `resolve`, `expand`,
  `neighbours`, `list_skills`, `list_instances`,
  `run_stored_query`, `validate`, `open_session`, `apply_op`,
  `close_session`, `update_page`). Tools are store-agnostic.
- The Tier-1 token budget arithmetic: 189 tokens invariant
  across 10²→10⁶ instances. This was a function of skill
  count, not storage shape.
- The typed-backlinks latency (5.21 ms p95 at 100 k instances
  per skill, 19× headroom). Backlinks is a pure relational
  query; the number is robust to the consolidation.
- The DuckLake origin axis. Already DuckDB-native.

## Pre-deployment gate

The consolidation accepts a measurable vector-search latency
increase relative to the prior two-store baseline. The gate
before any production deployment is one spike, scoped at 1–2
hours of implementation plus ~30 minutes of measurement:

> Run the 460-block / 30-query retrieval evaluation and the
> 10,120-block synonym-mutant stress evaluation against a
> DuckDB build using the `vss` HNSW index plus the `fts` BM25
> index plus RRF fusion. Compare nDCG and p50/p95 latency
> against the Lance baseline already captured.

Acceptance criteria, declared in advance:

| metric | Lance baseline | DuckDB target | failure path |
|---|---|---|---|
| nDCG @ 460 blocks | 0.975 | ≥ 0.95 | retune VSS HNSW parameters (`ef_construction`, `ef_search`); retest |
| nDCG @ 10,120 blocks (vector) | 0.933 | ≥ 0.90 | as above |
| nDCG @ 10,120 blocks (FTS) | 0.350 | ≥ 0.60 | tune FTS tokenizer (stemmer, k1, b); if still below, fall back to a hybrid lane (keep DuckDB for vector + relational + CRDT and attach an external FTS engine for the lexical column) — note this regresses part of the consolidation |
| p50 vector search latency, 460 blocks | 4.3 ms | ≤ 15 ms (≈3.5× tolerance) | accept; this is the engineered cost |
| p95 vector + skill-filtered search, 100k instances | 18.71 ms | ≤ 50 ms (≈2.7× tolerance) | accept; the pure-SQL path may also be faster than the prior Arrow-handoff baseline |

If the FTS nDCG target fails and tokenizer tuning does not
close the gap, the fallback is **not** to revert the whole
consolidation; it is to keep one separate FTS engine attached
as a single retrieval column, with vector + relational still
consolidated in DuckDB. That preserves most of the
simplification.

## References

- [`../spec/storage.md`](../spec/storage.md) — implementation
  of the consolidated layout.
- [`../spec/roadmap.md`](../spec/roadmap.md) — M1 spike bullet
  references this gate; the FTS-quality fallback is also tracked
  there.
