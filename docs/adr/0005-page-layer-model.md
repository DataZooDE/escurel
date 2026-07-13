# ADR-0005 — The page `layer` model (pinned base vs editable overlay)

**Status:** Accepted, 2026-07-13.
**Scope:** intra-node stability enforcement — every page carries a
**layer**: `overlay` (tenant-authored, editable; the default) or
`base@<pack>@<version>` (imported from a subscribed skill pack,
read-only at this node). This ADR covers the field, its write
enforcement, and its `list_skills` surfacing. The pack objects
themselves (export/import/subscription, the promotion gate) are
follow-on ADRs; the layer model is deliberately shippable alone and a
tenant with no packs behaves exactly as before (INV-ISO).

## Context

The interlocked-loops delivery model (see the 2026-07-13 fit-gap
analysis in the research repo) needs escurel to keep the reusable,
firm-authored knowledge substrate — canonical skills, data models,
edge-case template libraries distributed as versioned **skill packs** —
**stable at the customer node**, while the tenant's own onboarding and
runtime pages stay fully editable. That is the 70-20-10 principle
turned into an enforcement question: the tiers differ precisely in how
stable they are, and until now escurel had no way to express that
difference *within* one tenant. Writability was keyed strictly on
backend *kind* (`Capabilities::for_kind(...).writable` →
`Issue(backend_read_only)`), so two markdown pages could never differ
in writability.

## Decision

1. **A `layer` frontmatter field**, untyped like all frontmatter.
   Absent ⇒ `overlay` (every pre-layer page is an overlay page; no
   migration). Pack import stamps `layer: base@<pack>@<version>` on
   every page it lands.
2. **Enforcement lifts the existing read-only-backend seam from
   per-backend-kind to per-page-layer** — the same dispatch point in
   `tool_update_page`, a sibling guard
   (`Indexer::layer_read_only_rejection`) beside
   `backend_read_only_rejection`, and a typed
   `Issue(layer_read_only)`. No second enforcement mechanism.
   The guard is fail-closed in both directions:
   - it keys off the **stored** page's layer, so stripping `layer:`
     from a draft is not an unlock;
   - a draft **declaring** `layer: base@…` is rejected too — base
     pages are created by the pack-import path only, so an agent can
     neither squat a page id a future import lands on nor launder
     agent-authored content as pack-authored.
3. **`open_session` on a base-layer page is rejected** (JSON-RPC
   `-32000`, message prefix `layer_read_only:`) — the live CRDT
   co-authoring path must not bypass the whole-page guard.
4. **`list_skills` reports each skill's `layer`** (additive wire
   field, `"overlay"` default) so agents and operators can tell the
   stable substrate from editable pages without a second call.
5. **Internal writers are unaffected.** `seed_from_dir`, the
   ingest/materialise pipelines, and the future pack import call
   `Indexer::update_page` directly, below the dispatch-seam guard —
   deliberately, because they are the paths that legitimately create
   and refresh base pages (and `seed_from_dir` is how production
   `ESCUREL_SEED_DIR` seeding already works).

Specialisation without forking — a tenant overlay page shadowing a
base skill with the base value exposed for drift visibility — is part
of the pack-import ADR (it needs base pages to exist first); the layer
model here is its prerequisite.

## Consequences

- The 70% tier (imported packs) is enforceable at rest: a spoke
  cannot drift its base layer by accident or by agent action, so a
  future pack upgrade (`rebase`) has a pristine base to diff against.
- Derivability holds: `layer` lives in canonical frontmatter, so
  `rebuild`/`audit` need no new inputs.
- A base page can still be *removed* by an admin filesystem operation
  or re-landed by re-import — read-only applies to the agent write
  surface (`update_page`/`apply_op`), which is the boundary that
  matters for unattended loops.
- Tests: `crates/escurel-server/tests/layer_read_only.rs` (AT-LAYER-1,
  the strip-the-field unlock attempt, the spoof guard, AT-LAYER-3
  no-regression, `list_skills` surfacing, the `open_session` bypass).
