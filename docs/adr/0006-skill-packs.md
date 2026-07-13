# ADR-0006 — Skill packs: the signed unit of knowledge distribution

**Status:** Accepted, 2026-07-13.
**Builds on:** [ADR-0005](0005-page-layer-model.md) (the `layer` model —
packs land at a spoke as the read-only base layer) and
[ADR-0003](0003-capture-webhook-hmac-auth.md) (the HMAC-SHA256 signing
pattern, generalised from webhook POSTs to at-rest bundles).
**Scope:** the pack object and its export (`export_pack`). Subscribe/
import at the spoke and the promotion gate are follow-on ADRs.

## Context

The interlocked-loops model needs a **Company Model**: firm-authored
skills, canonical data models, and edge-case template libraries that
compound across engagements. Its unit of distribution must be
versioned (a spoke pins what it runs), verifiable (an air-gapped spoke
must check integrity + publisher offline), secret-free (it crosses
trust zones), and derivable (a spoke rebuilds identically from its
overlay + the pinned pack). escurel already had the bundle mechanism —
`tenant_export`'s tar+gz of canonical markdown — but no selectivity,
no identity, no signature.

An OKF (open knowledge format) interop layer was proposed in research
as the pack wire format, but has **not landed** in this repo; packs
therefore ride the existing tar+gz machinery. If OKF lands later, a
pack gains an alternative serialisation behind the same manifest.

## Decision

1. **A pack = a deterministic tar.gz of a skill subtree + a signed
   `pack.manifest.json`.** Entries are the selected skills' pages
   (plus, opt-in, their instances) under lane-relative paths
   (`skills/<id>.md`, `instances/<skill>/<id>.md`); headers pin
   mtime/uid/gid/mode so unchanged content re-exports byte-identically
   (packs are content-addressed).
2. **The manifest binds identity → bytes**: `{format_version, id,
   version, vertical, publisher, page_count, content_hash, signature}`.
   `content_hash` = `sha256:<hex>` over the tarball. `vertical` is
   load-bearing (REQ-PACK-03): convergence holds only within a
   vertical, and the importer will guard on it.
3. **Signing = HMAC-SHA256 with a shared `ESCUREL_PACK_SECRET`** over
   the canonical manifest JSON with `signature` emptied (struct field
   order fixes the bytes). The signature covers `content_hash`, so
   verifying is: authenticate manifest, then hash the tarball. Hub and
   spokes are firm-operated, so a shared secret is the v1 trust model;
   asymmetric signatures (per-publisher keys, revocation) are a
   follow-on that slots behind `verify_pack` without changing the
   manifest shape.
4. **Fail-closed everywhere**: no configured secret → `export_pack`
   refuses (packs are signed, always); a credential-shaped string (DSN
   with inline credentials, PEM private key) in any selected page →
   the whole export aborts (`pack_secret_detected`, INV-SECRETFREE).
   The scrub lives in `escurel-index::pack` and is deliberately
   deterministic and small; the promotion gate (WI-4) extends the same
   deny set rather than growing a second scrubber.
5. **Surface**: admin MCP tool `export_pack` + CLI
   `escurel admin pack export` (tool↔CLI parity ratchet). The exporter
   does not stamp `layer:` — the **importer** stamps
   `base@<id>@v<version>` on landing, because layer is a property of
   where a page sits, not of the page itself.

## Consequences

- A hub can publish; nothing can consume yet — `import_pack` /
  subscriptions arrive next and re-cover tamper rejection end-to-end
  over `/mcp` (`verify_pack` is already written and unit-tested against
  real bundles: tamper, forged-manifest, wrong-secret).
- Determinism gives INV-DERIV a concrete artefact: re-exporting an
  unchanged corpus yields the same `content_hash`, so pack diffs are
  meaningful.
- Tests: `crates/escurel-server/tests/pack_export.rs` (conformant
  signed pack, determinism, fail-closed unsigned export, fail-closed
  secret scrub, admin gate) + `escurel-server/src/pack.rs` unit tests
  (verify round-trip, tampered byte, forged manifest, wrong secret).
