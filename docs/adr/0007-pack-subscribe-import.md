# ADR-0007 — Pack subscribe/import: the pinned base layer at the spoke

**Status:** Accepted, 2026-07-13.
**Builds on:** [ADR-0005](0005-page-layer-model.md) (the `layer` model)
and [ADR-0006](0006-skill-packs.md) (the signed pack object). This ADR
is the consuming half: a spoke turns a published pack into its pinned,
read-only **base layer** — the L3→L2 coupler of the interlocked-loops
model ("the next engagement starts at `base@vN`, not empty").

## Decision

1. **`import_pack` is transport-neutral and offline-first
   (INV-AIRGAP).** The caller supplies the manifest + tarball bytes;
   an air-gapped spoke imports a tarball carried on media with the
   same call a connected spoke would use after fetching. No capability
   requires a live hub; a "live pull" is a future convenience wrapper,
   not a protocol feature.
2. **Trust before touch.** `verify_pack` (HMAC signature, then
   `content_hash`) runs before a single tar entry is read. Entry
   paths are validated fail-closed against zip-slip shapes (relative,
   no `.`/`..`/empty segments, `.md` only) — the `tar` crate refuses
   to *write* such paths but an attacker's tarball is not written by
   our builder.
3. **Pages land under the reserved `markdown/base/<pack>/` namespace**,
   stamped `layer: base@<id>@v<version>`. The agent write surface
   (`update_page`, `open_session`) rejects the entire prefix
   **statically** — even page ids no import has landed yet — which
   closes the layer-model review's TOCTOU and squatting findings
   without a lock: there is no window between guard and write because
   the guard doesn't depend on state. Pack pages must arrive
   layer-free; the importer stamps (layer is a property of where a
   page sits, and a pack shipping its own `layer:` is malformed).
4. **The subscription pin is a canonical DuckDB table**
   (`pack_subscriptions`, one row per pack), following the
   `external_credentials` precedent: a separate canonical input that
   `rebuild` never drops (INV-DERIV — the tenant rebuilds from overlay
   pages + subscribed packs). The row is written LAST, so a failed
   import never leaves a pin claiming pages that didn't land.
5. **The pinned version never moves silently** (the paper's
   unattended-loop accountability rail at the federation edge):
   re-importing the pinned version is idempotent (page upsert +
   `REPLACE` pin); any other version refuses `pack_version_pinned` —
   upgrades are a future explicit, reviewed `rebase`.
6. **Vertical guard (REQ-SUB-03).** Convergence holds only within a
   vertical, so subscribing across verticals refuses
   (`vertical_mismatch`) unless the operator passes the loud
   `allow_vertical_mismatch` escape hatch.
7. **Surface:** admin MCP tools `import_pack` + `list_packs`; CLI
   `escurel admin pack import` (manifest defaults to
   `<in>.manifest.json`) and `escurel admin pack list`.

`SCHEMA_VERSION` bumps 6 → 7 (the new table); existing tenant DBs gain
it on next boot via `ensure_pack_subscriptions` (idempotent, like the
credential registry).

## Consequences

- The federation loop's subscribe direction is closed end-to-end and
  tested as such: the acceptance test runs a real **hub** gateway and a
  real **spoke** gateway and moves a signed pack between them over
  `/mcp` (`crates/escurel-server/tests/pack_import.rs`).
- Overlay-shadows-base resolution (`base.<field>` drift visibility,
  AT-LAYER-2) is deliberately NOT in this ADR — it needs merge
  semantics on the read path and lands separately; today a base skill
  and a tenant overlay page are distinct pages with distinct ids.
- The harvest direction (promotion + scrub gate) is next; the reviewed
  `rebase` for version upgrades after that.
