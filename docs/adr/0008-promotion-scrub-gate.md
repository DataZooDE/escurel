# ADR-0008 — The promotion + scrub gate (the L2→L3 harvest)

**Status:** Accepted, 2026-07-13.
**Builds on:** [ADR-0006](0006-skill-packs.md) (packs, the shared
scrub deny set) and [ADR-0007](0007-pack-subscribe-import.md) (the
subscribe direction). This ADR closes the harvest direction — the
security-critical seam of federation, the analogue of INV-ACL-FUSION:
one boundary where a mistake leaks customer-confidential data into a
substrate other customers consume.

## Context

The interlocked-loops model's defensibility rests on the L2→L3→L2
sub-loop: the reusable fraction of a solved engagement deposits into
the Company Model and shortens every later engagement. Mechanically
that means content crossing the tenant boundary outward — exactly what
every other escurel invariant exists to prevent. The gate must
therefore be **fail-closed, default-deny, human-governed, and
audited**, and a zero-leakage regression test ships with it, not after.

## Decision

1. **Default-deny eligibility (REQ-PROMO-01).** `submit_promotion`
   accepts only ids that resolve to tenant-authored **skill pages**
   carrying the curator-set `promotable: true` marker. Raw instance
   data never promotes (the default policy — no anonymized-aggregate
   path exists yet; it stays contract-gated future work). Base-layer
   pages never promote (they are the hub's; re-promoting them would
   launder provenance). One ineligible id refuses the WHOLE request —
   no silent partial harvest.
2. **The marker is curator-set (AT-PROMO-2).** A non-admin
   `update_page` whose draft carries a truthy `promotable` refuses
   (`promotable_requires_curator`) — an agent can neither self-promote
   a page nor keep the marker alive by re-writing a curated page. In
   the v1 two-role model (Agent | Admin), "curator" = Admin; a
   dedicated curator role slots in when role vocabulary grows.
3. **The deterministic scrubber is the export deny set** — one place
   decides "this must not leave the node" (`escurel-index::pack::
   pack_scrub_rejection`: DSNs with inline credentials, PEM/PGP
   private keys, `password=`-style connection strings; grown by
   review, pinned by tests). One hit aborts the whole submission.
4. **Maker/checker (REQ-PROMO-03).** The tool *proposes*: it emits a
   signed candidate bundle with `version: 0` and publisher
   `spoke.<tenant>`. A human curator at the hub reviews the candidate
   and publishes deliberately (the hub's own `export_pack` under a
   real version). Nothing auto-publishes; there is no code path from
   submission to a subscribable pack.
5. **Every submission is an immutable audit event (REQ-PROMO-04)** —
   `source: "promotion"`, the page list in the body, the submitting
   subject and candidate `content_hash` in provenance. "What left this
   spoke, when, approved by whom" is replayable and contract-grade.
6. **Surface:** admin MCP tool `submit_promotion`; CLI
   `escurel admin pack submit-promotion` (writes candidate tarball +
   manifest for carry to the hub — air-gap-compatible like import).

## Consequences

- The paper's L2→L3→L2 sub-loop is now mechanically closable:
  subscribe (ADR-0007) down, harvest (this ADR) up, with a human gate
  on each direction.
- The zero-leakage regression battery
  (`crates/escurel-server/tests/promotion_gate.rs`) is a mandatory
  gate, modelled on the fusion-ACL test: unmarked skills, instances
  (even ones maliciously tagged promotable), base-layer pages,
  credential-shaped content, and mixed eligible/ineligible requests
  all refuse; the emitted bundle holds exactly the eligible pages.
- Deliberately not built: the anonymized-aggregate path (REQ-PROMO-05,
  contract-gated, off by default), a dedicated curator role, hub-side
  candidate-review tooling (the hub operator uses the ordinary
  corpus + export tools), and the reviewed `rebase` for base upgrades
  (still the missing piece of the federation loop — the CRDT
  three-way-merge machinery the research assumed does not exist).
