# Loro 1.x: incremental update ops must come from a persistent
# client doc, not a per-op scratch doc

**Date:** 2026-05-25
**Scope:** `escurel-crdt` test design; future M4 transport code
that constructs Loro ops on the client side.

## Symptom

The M4.1 integration test `snapshot_then_close_then_reopen_replays_content`
asserted final content `"alpha beta"` after applying two `apply_op`
calls to a `LiveDoc`. Test fixture built each op from a fresh
scratch `LoroDoc` that imported a "mirror" state and then exported
`ExportMode::updates(&prior_vv)`. After both ops, the actor's doc
read `"alpha "` instead of `"alpha beta"`.

A sibling failure: `reopening_with_snapshot_plus_ops_replays_to_correct_state`
failed with a primary-key collision (`(page_id, op_id)`) after
reopening because the op-count counter restarted from zero post-snapshot.

## Cause

**Loro op anchors are peer-relative.** When a scratch `LoroDoc`
(peer Z) imports another doc's state (peer Y's `"alpha "`) and
then inserts `"beta"` at position 6, the exported update encodes
the insert as "after peer Y's 6th character". When that update
is imported into a *third* doc (the actor, which holds peer X's
`"alpha "` from a *different* mirror), the anchor can't resolve —
peer Y's character IDs aren't present, so the new content fails
to merge as expected.

The op-id collision was a related class of trap: deriving op-ids
from a per-actor monotonic counter that reset on reopen meant the
first `apply_op` after a reopen reused an id from a prior session.

## Fix in this PR

* **Tests use a persistent `Client` helper** with its own `LoroDoc`
  that lives across all ops in a test. Each `insert` exports
  `ExportMode::updates(&vv)` where `vv` is the client's own
  oplog-vv from the previous export — incremental, anchored to
  ids the actor has already seen.
* **`DuckdbCrdtBackend::max_hlc`** returns the highest hlc across
  both `crdt_ops` and `crdt_snapshots`; `LiveDoc::open` seeds its
  op-count from that, so reopen never reuses an op-id.

## How to recognise next time

* "I applied two ops, content shows the first but not the second"
  → check that the second op was generated against a client doc
  that **already had** the first op locally, not against a fresh
  scratch reimporting the previous mirror state.
* "Primary key collision on `crdt_ops`" after reopening a
  `LiveDoc` → the actor's op-count seed is not aware of prior
  sessions' rows; query `max(hlc)` from the persistence layer.

## Implication for M4.2+ transport

WS clients constructing Loro ops must keep one `LoroDoc` open for
the lifetime of the editing session and export incremental updates
from it. Constructing ops from a fresh `LoroDoc` per message (e.g.
naively in a stateless handler) will produce ops that the server's
`LiveDoc` cannot anchor.
