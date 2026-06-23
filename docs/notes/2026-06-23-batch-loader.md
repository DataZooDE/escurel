# Offline batch loader + DuckDB→DuckDB transfer

**Status:** implemented (`escurel-loader` crate + binary).
**Problem:** loading a large corpus (e.g. ~20k PDFs, ~6–10 M chunks) through the
live server's `POST /ingest/upload` is hopeless — each chunk is one embed, so the
per-tenant Embeds quota (300/min) alone makes it a multi-week trickle, and a live
Gemini run costs hundreds of dollars.

## The idea

Do the expensive work (extract → chunk → **embed**) **once, offline**, in a
throwaway *loader* escurel instance with its own DuckDB + blob dir, at full speed
with no HTTP and no quota. Then **transfer** the result into the live tenant
carrying the **embeddings as data**: copy blobs + overlay markdown into the live
LaneStore, and copy `pages`/`blocks`/`links` rows DuckDB→DuckDB via DuckDB's
`ATTACH` + `INSERT … SELECT`. Production never re-embeds.

```
OFFLINE (loader, no server/quota)            TRANSFER (host-side operator)
  src dir of PDFs/DOCX/text                    live tenant: escurel.duckdb + LaneStore
   └ Extractor (kreuzberg/plaintext)            validate manifest vs live embedder
   └ chunk_text                                 copy blobs + overlays  (files first)
   └ embed (EmbeddingGemma | hash | …)          attach_external(loader.duckdb, RO)
   └ materialise → loader.duckdb + blobs/       DROP hnsw → INSERT…SELECT (skip
   └ manifest.json {model_id, dim, schema}        existing) → recreate hnsw → refresh_fts
           vectors copied verbatim (FLOAT[768]) — NO re-embed in prod
```

## Why this surface, not MCP / tenant_export

The transfer needs both DuckDB files co-located on disk for `ATTACH`, and the
offline build is a multi-hour in-process job — both are host-side operator work,
wrong for the one-shot MCP transport. `tenant_export`/`import` ship markdown only
and **rebuild + re-embed** on import — the exact cost we set out to avoid. So the
loader is its own binary (`escurel-loader build|transfer`), not an `escurel-cli`
subcommand (the CLI is a pure HTTP-client presentation layer that can't `ATTACH`
files).

## The compatibility gate (why it can't silently corrupt)

Vectors are only valid if the loader used the **same embedding model + dim (768)**
as the live tenant — mixing embedding spaces silently destroys retrieval. The
loader records `model_id` + `dim` + `schema_version` in `manifest.json`; the
transfer validates all three against the live tenant's `--expect-model` and this
binary's `Migrator::SCHEMA_VERSION` **before** touching anything, and fails closed
on mismatch. (`Embedder::model_id()` was added to the trait for exactly this.)

## Idempotency + crash-safety

- Per-document `instance_id` = the file's content sha256 → identical files dedupe,
  and the document `page_id` is deterministic, so a re-run resumes cleanly.
- `--on-collision skip` (default) imports only new `page_id`s; `replace`
  delete-then-inserts colliding ones; `error` aborts if any collide.
- **Files first, rows last.** Blobs + overlays are content-addressed / key
  deterministic (idempotent), copied before the DuckDB rows land in one
  `BEGIN…COMMIT`. A crash mid-transfer leaves at worst orphan blobs that
  `audit_documents`/`reclaim_orphan_blobs` sweep — never rows referencing missing
  content.

## Bulk-load mechanics

See the discovered note
[`2026-06-23-loader-transfer-hnsw-fts-and-fresh-migrate.md`](discovered/2026-06-23-loader-transfer-hnsw-fts-and-fresh-migrate.md):
HNSW is dropped before the bulk insert and recreated after (per-row HNSW
maintenance is the slow path; a cosine scan stays correct without it); the BM25
FTS snapshot is refreshed once; and `Migrator::up` runs only for a fresh target
DB (it is not idempotent — a transfer into an existing tenant must not re-CREATE
the tables).

## Out of scope

Row-grain SQL backends; a LanceDB hatch; a server-side batch job. The
`tooling/bulk-ingest` Python uploader stays for trickle / live loads. A real
Gemini Batch-API embedder (async submit/poll, ~half price) is a natural next
step — the loader's `--embedder` already abstracts the embedding space, and the
manifest gate makes adding `gemini-batch:<model>` safe.
