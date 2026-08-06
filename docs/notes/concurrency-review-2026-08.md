# Concurrency review: optimistic writes, conflict resolution, CRDT

**Date:** 2026-08-06. Three independent reviews (Claude Fable 5), run in
parallel over disjoint scopes. Every load-bearing claim below was verified
directly against the code; claims I could not confirm are marked as the
reviewer's, not mine.

## Baseline

| area | source | real test cases |
|---|---|---|
| optimistic concurrency (`tool_update_page`) | 321 lines | 5 |
| conflict resolution (`tool_rebase_pack`, `pack.rs`) | 297 + 572 | **2** |
| CRDT (`escurel-crdt`) | 1,551 | 17 |

The thinnest coverage sits on the code most able to destroy a user's work.

---

## Finding 1 — `expand` mixes two stores; a committed session edit can be
## silently overwritten *(highest severity, verified)*

`expand` builds its response from **two different stores**:

| field | source |
|---|---|
| `body`, `blocks` | `indexer.expand(...)` — blocks table / LaneStore (`tools_read.rs:216`) |
| `version` | `backend.max_hlc(...)` — the CRDT store (`tools_read.rs:257`) |

`close_session(commit = true)` reaches `SessionManager::close`, which calls
`entry.doc.close(commit)` and **nothing else** (`session.rs`). `LiveDoc::close`
writes a CRDT snapshot; no path calls `indexer.update_page`.

So after a session commits:

1. `expand.version` advances (CRDT hlc moved).
2. `expand.body` does not (the indexer was never written).
3. A client reads that pair, edits, and calls `update_page` with the
   `base_version` it just received.
4. `base == head`, so the conflict branch is skipped and **no merge is
   attempted** (`tools_write.rs:204`).
5. The write overwrites the indexer content and the next snapshot overwrites
   CRDT history — discarding the committed session edits.

**Verified independently:** the two sources, the absence of any indexer write
in the close path, and — decisively — that nothing closes the gap elsewhere.
`escurel-crdt/src/reconciler.rs` exists but solves the *opposite* direction
(external markdown edits versus the snapshot), and `grep -rn reconcil` across
`escurel-server/src` returns **nothing**: the reconciler is not wired into the
server at all.

This also contradicts the gate's own doc comment, *"the tenant write path is
single-writer by design"* (`server.rs:337`). It is not: `apply_op` and
`close_session` are a second, ungated writer into the same version space,
neither of which takes `update_page_gate`.

**What is sound** (also verified): two concurrent `update_page`/`delete_page`
calls *are* correctly serialised — both take the same mutex before reading
`max_hlc` and hold it through the write and version bump, proven by
`simultaneous_stale_writes_serialize_under_the_gate`. The runner cascade is
safe for the same reason: it writes through the ordinary `/mcp` tool. The
defect is specific to the CRDT session path.

## Finding 2 — rebase is structurally blind to upstream deletions of
## shadowed skills *(verified)*

`tool_rebase_pack`'s conflict scan iterates `&stamped_pages` — the **incoming**
pack version only (`tools_admin.rs:1486`). A skill present in vN but dropped
in vN+1 therefore never enters the loop and is never checked against an
existing tenant overlay.

The apply phase then removes it as an orphan and only increments
`pages_removed` (`tools_admin.rs:1640-1648`) — **no issue is raised**.

Result: a tenant overlay that shadows a base skill, whose base is dropped
upstream, produces `ok: true` with zero issues, leaving the overlay dangling.
No bytes are deleted — `remove_page` is scoped to the reserved base prefix, so
overlay content is safe — but every base-inherited field the overlay did not
itself override silently disappears from what an agent reads.

The scan cannot see this case by construction: it is keyed off the new
tarball rather than the union of old and new.

**Also verified:** the orphan computation is duplicated verbatim between the
dry-run path (`tools_admin.rs:1594`) and the apply path (`:1626`) — two copies
that must agree and nothing that makes them.

## Finding 3 — CRDT convergence has never been tested *(verified)*

`LoroDoc::new()` appears **exactly once** in the entire CRDT test suite
(`livedoc_roundtrip.rs:53`). Every test drives a single peer through one
actor, so what the tests prove is that the actor serialises calls — not that
two independent peers converge.

Merge itself is correctly delegated to Loro (`livedoc.rs:186-203` imports op
bytes and does nothing else); there is no hand-rolled merge to diverge from
Loro's semantics. So this is a testing gap rather than a known defect — but
the crate's central correctness property is currently taken on faith.

**Also verified:** `backend.rs:63` documents that `snapshot` is called "on
session close *and on periodic checkpoints*". The only call site is
`handle_close` (`livedoc.rs:222`). **The periodic checkpoint does not exist.**
A long-lived session therefore accumulates an unbounded op tail with no
intermediate snapshot, and crash recovery replays all of it. This is the same
defect class the repository has been bitten by before: documentation asserting
behaviour the implementation does not have.

## Recommended order

1. **Finding 1** — decide the contract between the CRDT store and the indexer.
   Either `close_session(commit)` writes back through `indexer.update_page`, or
   `expand` hydrates `body` from the CRDT snapshot when one is newer. Do not do
   both. Until then, `expand`'s `version` and `body` can disagree, and the
   optimistic-concurrency protocol built on that pair is unsound whenever a
   session is in play.
2. **Finding 2** — extend the scan input to `old ∪ new` page ids and raise a
   typed issue for a removed-but-shadowed skill; extract the duplicated orphan
   computation while there.
3. **Finding 3** — add a two-peer convergence test and a property test over
   random op interleavings. Test-only, zero production risk, and it validates
   the claim the whole collaborative-editing feature rests on.
4. Decompose `tool_update_page` (321 lines) and `tool_rebase_pack` (297) into
   named steps. Both reviewers independently observed that the size is *why*
   these gaps survived review — the concurrency reasoning is not legible.

## Tests to add, by name

*Optimistic concurrency* — `live_session_commit_is_visible_to_expand`,
`update_page_with_matching_base_after_session_commit_does_not_clobber_it`,
`concurrent_apply_op_and_update_page_produce_no_duplicate_hlc`.

*Conflict resolution* — `rebase_flags_upstream_deleted_page_that_a_shadow_overrides`,
`rebase_of_a_renamed_skill_with_new_slug_and_a_shadow`,
`crash_mid_apply_is_resumable_by_rerunning`.

*CRDT* — `two_independent_peers_converge_via_import`,
`prop_random_op_interleavings_converge`,
`periodic_checkpoint_bounds_op_tail_without_close`.

The first two in each group would fail today. That is the point of listing
them: they are not coverage for its own sake, they are the assertions that
would have caught these findings.
