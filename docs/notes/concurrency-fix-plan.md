# Fix plan for the three concurrency findings

**Date:** 2026-08-06. Against
[`concurrency-review-2026-08.md`](concurrency-review-2026-08.md), whose
findings were each verified directly against the code.

**Standing rule for this plan.** Every fix lands as a failing test first. All
three findings survived review precisely because nothing asserted the
property they violate; a fix without a test that would have caught the
original defect leaves the next regression undetected. Where a test cannot be
written before the fix (F1's contract does not exist yet), the plan says so.

Severity order is F1, F2, F3. F1 can lose a user's work today; F2 degrades
what an agent reads without a diagnostic; F3 is an untested property plus a
documentation lie.

---

## F1 — the CRDT store and the indexer disagree

### The decision that has to come first

`close_session(commit = true)` writes a CRDT snapshot and never touches the
indexer. Two repairs are possible and they are mutually exclusive.

**Option A — the commit writes through to the indexer.** `close_session`
persists the merged body the way `update_page` does. The indexer stays the
single source of truth; the CRDT store becomes collaboration transport plus
history.

**Option B — `expand` hydrates `body` from the newer CRDT snapshot.** No new
write path; the read side reconciles the two stores on the fly.

### Why Option A, decisively

Option B fixes the symptom that was noticed and leaves the disease. The
indexer does not merely serve `expand.body` — it owns **`blocks`** (which
feed BM25 and the vector index) and **`links`** (which feed `neighbours` and
the typed backlink graph). If a committed session never reaches the indexer,
then search cannot find the edit, backlinks do not see new wikilinks, and the
link graph silently lags the content. Hydrating `expand` would paper over one
read path while retrieval stayed wrong, and would leave two sources of truth
permanently — a state this codebase has already paid to escape once
(ADR-0001 consolidated two stores into one for exactly this reason).

Option A also restores the invariant the code already claims: *"the tenant
write path is single-writer by design"* (`server.rs:337`). Today that comment
is false. Under A it becomes true.

**Cost of A, stated honestly.** `close_session` becomes a write, so it must
take `update_page_gate`, re-run the write guards and ACL that `update_page`
runs, emit the same events, and handle failing *after* the CRDT commit
already succeeded. That last case is new and needs a decision of its own:
either the CRDT commit is deferred until the indexer write succeeds, or a
failed indexer write leaves a committed snapshot that a later reconcile must
pick up. **Prefer deferring** — commit the CRDT snapshot last, so a failure
leaves the session closable and retryable rather than half-applied.

### Work items

**F1.1 — Decompose `tool_update_page` (321 lines) first.** Not cosmetic: the
fix needs to *reuse* the write tail, and it cannot be reused while it is
inlined in a 321-line function. Extract, in the same module:

- `resolve_base_version(...) -> Cas` where `Cas` is an explicit
  `Clean { head_hlc } | Merged { content, head_hlc } | Conflict(Value)`.
  Naming the outcome is what makes "what does the gate decide" legible.
- `persist_and_bump_version(...)` — the indexer write, the version bump, the
  metrics and the event emission.

Risk: low. Pure refactor inside one gate scope, covered by the five existing
concurrency tests.

**F1.2 — Route the session commit through `persist_and_bump_version`.**
`close_session(commit = true)` takes `update_page_gate`, exports the merged
body from the LiveDoc, and persists it through the same tail. Order: indexer
write, then version bump, then CRDT snapshot last.

Risk: **high, and the highest in this plan.** It serialises session closes
against whole-page writes, changes the failure mode of `close_session`, and
touches the one code path that already caused a production data-loss
incident. It needs the F1.4 tests green before and after.

**F1.3 — Unify the version space.** `apply_op` seeds its op counter from
`max_hlc` once at `LiveDoc::open`; a concurrent `update_page` advances the
hlc invisibly to that actor. Either take the gate for `apply_op` too, or make
the hlc allocation go through one authority. Prefer the latter — gating every
op would serialise live typing against whole-page writes and is likely
unacceptable for interactive editing.

Risk: medium. Needs a decision on where hlc allocation lives.

**F1.4 — Tests, written before F1.2.**

- `live_session_commit_is_visible_to_expand` — fails today.
- `update_page_with_matching_base_after_session_commit_does_not_clobber_it` —
  fails today; this is the data-loss assertion.
- `search_finds_a_committed_session_edit` — fails today, and is the test that
  proves Option B would have been insufficient.
- `concurrent_apply_op_and_update_page_produce_no_duplicate_hlc` — guards
  F1.3.

---

## F2 — rebase is blind to upstream deletion of a shadowed skill

Smaller, self-contained, and fixable without a design decision.

**F2.1 — Test first.**
`rebase_flags_upstream_deleted_page_that_a_shadow_overrides`: import v1,
shadow `beta` with a local overlay, rebase to v2 (which drops `beta`), assert
a typed issue is present. Passes today with `ok: true` and zero issues, so
write it asserting the *desired* behaviour and watch it fail.

**F2.2 — Widen the scan input.** The conflict scan iterates
`&stamped_pages`, the incoming version only (`tools_admin.rs:1486`). Change
its input to the union of old and new base page ids so a removed-but-shadowed
skill is reachable, and raise a `rebase_orphaned_shadow` issue carrying the
overlay's page id and the skill it shadowed.

Gate it like the existing `rebase_conflict`: blocking unless
`acknowledge_conflicts` is set. An operator who knows the skill is gone can
proceed; one who does not, finds out.

**F2.3 — Extract `orphaned_base_pages(old, new)`.** The computation is
duplicated verbatim between the dry-run (`:1594`) and apply (`:1626`) paths.
Two copies that must agree, with nothing making them. Near-zero risk.

**F2.4 — Decompose `tool_rebase_pack` (297 lines)** into
`verify_and_stamp_incoming`, `check_skill_collisions`,
`detect_shadow_conflicts`, `orphaned_base_pages`, `apply_rebase_pages`,
`commit_rebase_pin`. The case analysis is currently unreadable, which is why
the deletion case was missed.

**F2.5 — `crash_mid_apply_is_resumable_by_rerunning`.** The resumability
claim rests entirely on code comments. Either prove it or discover it is
false.

---

## F3 — CRDT: an untested property and a false doc comment

**F3.1 — `two_independent_peers_converge_via_import`.** Two separate
`LoroDoc`s, local inserts on each, exported and imported into one another out
of order; assert both bodies are identical. `LoroDoc::new()` currently appears
exactly once in the whole suite, so nothing tests convergence today.

**F3.2 — `prop_random_op_interleavings_converge`.** N peers, random ops,
random pairwise exchange order, assert all converge. This is the textbook CRDT
property the collaborative-editing feature rests on and it is presently taken
on faith. Test-only, zero production risk.

**F3.3 — The periodic checkpoint: implement it or delete the claim.**
`backend.rs:63` documents snapshots "on session close *and on periodic
checkpoints*"; the only call site is `handle_close`. Two honest repairs:

- **Correct the doc** (minutes, zero risk) — accurate immediately, leaves the
  unbounded op tail on long-lived sessions.
- **Implement the checkpoint** (real work) — snapshot every N ops or T
  seconds inside the actor loop, bounding crash-recovery replay and
  `crdt_ops` growth.

**Do the doc correction now and the implementation on its own merits.** A
comment that describes behaviour the code lacks is the defect; the missing
feature is a separate, arguable question. Shipping the doc fix immediately
also stops the next reader designing against a checkpoint that isn't there.

---

## Sequencing

| # | item | risk | status |
|---|---|---|---|
| 1 | F3.3 doc correction | none | **done** — `backend.rs` now records that the periodic checkpoint does not exist |
| 2 | F2.1 + F2.2 + F2.3 | low | **done** — `rebase_orphaned_shadow` issue; `orphaned_base_pages` extracted from its two verbatim copies |
| 3 | F3.1 + F3.2 | none | **done** — `crates/escurel-crdt/tests/convergence.rs` (4, incl. the property test over random interleavings) |
| 4 | F1.1 decomposition | low | **done** — `resolve_base_version` → `Cas { Clean, Merged, Conflict }` |
| 5 | F1.4 tests | none | **done** — verified red first; see the note below, they were nearly not |
| 6 | F1.2 session commit write-through | **high** | **done** — Option A, indexer first, CRDT snapshot last |
| 7 | F1.3 version-space unification | medium | **done** — the backend allocates; `append_op_next` / `snapshot_next` |
| 8 | F2.4, F2.5, decompose rebase | low | **done** — 297 → 73 lines in six named steps; resumability proven |

### Deviations from the plan as written

- **F1.3 did not become a per-page gate.** The plan (and the decision taken
  on it) assumed a new lock keyed by page id. It turned out no new lock was
  needed: the DuckDB backend's connection mutex is already the serialization
  point, and the reason the old code raced is that it took that lock *twice*
  — `max_hlc`, then `append_op` — not that it lacked one. Allocating inside
  the single critical section is strictly stronger than a per-page gate
  around two, and adds nothing to hold.
- **The close-time snapshot deliberately does not allocate.** First
  implemented as `snapshot_next` for symmetry, which broke
  `snapshot_then_close_then_reopen_replays_content` (v2 → v3). Correctly so:
  a snapshot records "the state as of op N" rather than being a new event, so
  allocating there advances the head with no content change and turns the
  version a client just received from `apply_op` into a stale
  `base_version`.
- **F1.2 did not reuse a `persist_and_bump_version` helper.** The write tail
  of `tool_update_page` is entangled with `UpdatePageArgs` — provenance,
  edit-event suppression, absorption metrics — none of which a session commit
  has. Extracting it would have meant inventing a parameter object to carry
  fields the caller does not have; `close_session` takes the gate and writes
  directly instead.

### On step 5, which is the part worth remembering

The first F1 test harness seeded content with `update_page` and all three
tests **passed**, having exercised none of the path — `update_page` writes
the indexer itself, so the two stores never diverged. Driving the page
through `apply_op` with a real Loro op made two of them red immediately; the
third was still green on an assertion that accepted the page id, which is
present either way. A test written to be red and found green is evidence
about the test until you have checked which. See
`docs/notes/discovered/2026-08-06-two-stores-one-version.md`.

### Still open

- **The periodic CRDT checkpoint (F3.3, implementation half).** The doc lie
  is fixed; the missing feature is not. A long-lived session still
  accumulates an unbounded op tail that crash recovery replays in full.
  Argue it on its own merits.
- **`close_session` does not re-run `update_page`'s write guards.** The
  layer/backend read-only guards and validation run on the `update_page`
  path; a session commit writes through without them. `open_session` already
  refuses a `layer_read_only` page, so the main hole is closed, but this is
  narrower than "the same guards" and should not be described as equivalent.

## What could still be wrong with this plan

- **Option A may be unacceptable for interactive latency.** Serialising
  session closes against whole-page writes is fine; serialising *ops* (F1.3's
  rejected variant) would not be. If measurement shows close-time contention
  matters, the fallback is a per-page rather than per-tenant gate.
- **F1.2's failure ordering is asserted, not measured.** "CRDT snapshot last"
  is the right shape, but the first implementation should be tested against a
  forced indexer failure rather than assumed correct.
- **F2's fix changes a success into a blocked operation** for tenants who
  already rebased past a deleted shadowed skill. Their state is already wrong;
  the issue will surface on their next rebase, which is the point, but it will
  read as a regression to anyone who has not read this document.
