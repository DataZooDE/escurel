# A read that composes two stores needs one version authority

**Symptom.** A user edits a page in a live CRDT session, commits, and the
edit is gone the next time anyone writes the page. Not a merge conflict, not
an error — the client behaves correctly the whole way through and the work
disappears anyway.

**Mechanism.** `expand` built its reply from two different stores:

| field | source |
|---|---|
| `body`, `blocks` | `indexer.expand(...)` |
| `version` | `backend.max_hlc(...)` — the CRDT store |

`close_session(commit)` wrote only a CRDT snapshot. So a commit advanced the
`version` a client reads while leaving the `body` it reads stale. The client
then did the correct thing — read `expand`, edit, write back with the
`base_version` it was handed — and because `base == head`, the conflict
branch was skipped, no merge was attempted, and the write overwrote the
session's work.

Every individual component was right. The defect was in the *pair*: two
stores, and a version number that only one of them could move.

**Fix.** `close_session(commit)` now writes the merged body through to the
indexer before closing. The alternative — hydrating `expand.body` from the
newer snapshot — would have fixed the symptom and left the disease, because
the indexer also owns `blocks` (BM25 + vector) and `links` (neighbours,
backlinks): search still could not have found a committed edit.

A related instance of the same shape, fixed alongside: `update_page` and
`LiveDoc` each allocated hlc values from `max_hlc`, one at write time and
one from a seed captured at `LiveDoc::open`. Two allocators over one space
meant both could stamp *different content* with the *same version*. The
repair was to let the backend allocate — read the maximum and insert without
releasing the store lock in between. `max_hlc` followed by `append_op` is
two calls, and two calls are not one allocation.

## How to recognise it next time

Ask of any response object: **does every field come from the same store?**
If not, the one that answers "how fresh is this" must be moved by whatever
writes the others, or it is describing a state no reader can obtain.

Grep-level tell: a write path that updates store A and a read path that
reads its freshness marker from store B, with no call between them. Here,
`grep -rn "reconcil" crates/escurel-server/src` returned nothing —
`escurel-crdt/src/reconciler.rs` exists but solves the opposite direction
(external markdown edits vs the snapshot) and was never wired into the
server. An unused reconciler is a strong hint that two stores are drifting
with nobody watching.

## The testing lesson, which cost more than the fix

The first version of the red test seeded page content with `update_page` and
then committed the session. **All three tests passed** — while exercising
none of the path they existed to cover, because `update_page` writes the
indexer itself, so the two stores never diverged and the bug had nothing to
act on.

A test written to be red and found green is not evidence the bug is absent.
It is evidence about the test, until you have checked which. The fix was to
drive the page through `apply_op` with a real Loro op, so the content
reached the CRDT store and nowhere else; two of the three then went red
immediately. The third was still green on a slack assertion — it accepted
`"c1"`, the page id, which is present whether or not the edit landed.

Both mistakes have the same shape as the production bug: a step that looks
like it exercises a boundary while quietly routing around it. Cheap
guard — before trusting a red test, make the assertion fail *for the reason
you expect*, and confirm the fixture's precondition is what you think (see
the `beta` present / `gamma` absent preconditions in
`crates/escurel-server/tests/pack_rebase_resumable.rs`).

## References

- `docs/notes/concurrency-review-2026-08.md` — F1, F1.3
- `docs/notes/concurrency-fix-plan.md` — the Option A/B decision
- `crates/escurel-server/tests/session_commit_writes_through.rs`
- `crates/escurel-server/tests/hlc_single_authority.rs`
