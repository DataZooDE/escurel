# `update_page` must hold the write mutex *across* the embed call

**Date:** 2026-05-24
**Scope:** `escurel-index::Indexer::update_page`

## Symptom

Codex review of M2.1–M2.6 flagged a TOCTOU race in `update_page`:

> When two `update_page` calls for the same `page_id` overlap and
> the older content's embed call is slower, this await lets the
> newer update commit first and then the older call can acquire
> the mutex and overwrite `pages`, `links`, and `blocks` with
> stale content.

Demonstrable scenario:

1. Call A: `update_page(p, body_v1)`. Embed is slow (say 200 ms).
2. Call B (200 ms later, on a different task): `update_page(p, body_v2)`. Embed is fast (5 ms).
3. B's embed finishes → B locks → B writes `body_v2`. ✓
4. A's embed finishes → A locks → A writes `body_v1`. **Stale.**

The final on-disk state reflects A's content even though B was
the most recent intent.

## Cause

The PR-M2.1 indexer ran `embedder.embed(...).await` **before**
acquiring `self.conn.lock().await`. That looked like a clean
optimisation — a slow embedder doesn't block other tenant
connections. It is correct under the spec's production
concurrency model (`docs/spec/platform.md §Concurrency`), where
the per-tenant write-RwLock in `kb-server` enforces single-writer
semantics one level above the Indexer.

But the M2-stage Indexer is *only* `self.conn.lock()` — no
higher-level write-lock yet. Without the production wrapper,
moving embed outside the lock means concurrent writes to the same
page can interleave.

## Fix

Take the per-tenant mutex *first*, then run embed + write inside
it:

```rust
let mut conn = self.conn.lock().await;
let embeddings = self.embedder.embed(&[body_text.as_str()]).await?;
// … validate, format vector literal …
let tx = conn.transaction()?;
// pages / links / blocks writes
tx.commit()?;
```

Cost: a slow embedder now blocks reads + writes through the same
connection. Acceptable in the M2-stage indexer; the M3 server
will introduce the spec's read-pool + write-RwLock pattern, at
which point this lock can move back outside the embed (because
the per-tenant write-lock above the Indexer will preserve
ordering instead).

## What to revisit at M3

`docs/spec/platform.md §Concurrency` mandates:

- Read pool of N DuckDB connections (no mutex contention for reads).
- Per-tenant `RwLock<TenantWriter>` — writes serialise across all
  `update_page` calls for the tenant, before reaching the
  Indexer.

When that's in place, the Indexer can again run embed outside the
mutex — the per-tenant `RwLock::write()` upstream is the
serialisation point.

## How to recognise next time

Any `update_page` / index-mutation method that does
`async_resource.await` *before* taking the write lock is a
candidate for this class of race. The fix is always one of:

1. Take the lock first (simple, makes concurrent operations
   slower).
2. Validate a version/hash at commit time and retry on mismatch
   (more complex; needed for high-contention paths).
3. Serialise at a higher level (the spec's preferred answer).

Codex's review prompt of "stability — async cancellation safety,
race conditions" caught this; the audit/rebuild test would
*not* have surfaced it because that test path is single-task.
Periodic codex reviews on async mutation paths specifically
earn their keep.
