# ADR-0003 — Authenticated capture webhook (HMAC-SHA256 + tenant identity)

**Status:** Accepted, 2026-06-07.
**Issue:** [#147](https://github.com/DataZooDE/escurel/issues/147)
(escurel-agent-runner epic, work-item 3/14).
**Lands as:** the single gateway-side change of the
[agent-orchestration contract](../contract/agent-orchestration.md) — the
gateway produces, the runner verifies.

## Context

The opt-in capture webhook (M7,
[`crates/escurel-server/src/webhook.rs`](../../crates/escurel-server/src/webhook.rs))
fires a fire-and-forget `POST` of each new `capture_event` to
`ESCUREL_WEBHOOK_URL`. As shipped it carried **no authentication and no
explicit tenant**:

- **No trust anchor.** Any host that can reach the runner's
  `POST /trigger` could forge a capture event. The runner had only a
  provisional plain-shared-secret header (`X-Escurel-Webhook-Secret`,
  #146), which a passive network observer could replay verbatim and
  which says nothing about the body — a man-in-the-middle could keep the
  header and rewrite the event.
- **No tenant identity.** The payload is a serialized `Event`, which has
  no tenant field. The runner could not tell which tenant an event
  belongs to; #146 read a provisional `X-Escurel-Tenant` header with an
  empty fallback. The runner needs the authoritative tenant to scope the
  per-run JWT and the work it dispatches.

The runner is an *autonomous* processor that acts on these events (folds
event→state via `assign_event`/`update_page`, drives a harness
subprocess). It must trust the POST and know the tenant before it acts.

## Decision

Authenticate the webhook with a **shared-secret HMAC over the body** and
carry the **tenant identity in the payload**:

1. **Tenant in the payload.** The gateway injects `tenant_id` into the
   delivered JSON event object before sending. The gateway is
   single-tenant per `Indexer`, so `indexer.tenant()` is the
   authoritative source of truth; the runner reads `tenant_id` from the
   payload for `Trigger.tenant`, replacing the #146 header/empty
   placeholder. The payload is always present (signed or not).

2. **HMAC-SHA256 over the body** (when `ESCUREL_WEBHOOK_SECRET` is set).
   The gateway serializes the payload **once** to bytes, computes
   HMAC-SHA256 over those exact bytes under the secret, and sends it as
   the header `X-Escurel-Webhook-Signature: sha256=<hex>` (lowercase hex
   of the 32-byte digest), POSTing the *same* bytes with
   `content-type: application/json`. Serialize-once-sign-then-send the
   same bytes guarantees the receiver's recomputed HMAC over the raw
   request body matches (a re-serialization could reorder keys / change
   whitespace and break the signature).

3. **Runner verifies on the raw bytes before parsing.** The
   `POST /trigger` handler extracts the body as raw `Bytes`, verifies the
   signature against `ESCUREL_WEBHOOK_SECRET` with a constant-time
   compare (`hmac::Mac::verify_slice`) **before** any JSON parsing, and
   returns `401` on a missing or mismatched signature when a secret is
   configured. With no secret configured the POST is unsigned and
   accepted (dev path). Verifying before parsing also removes the #146
   extractor-ordering wrinkle (a malformed body was previously parsed,
   and so potentially rejected, ahead of the auth check).

The secret is symmetric and shared out-of-band (substrate
secret-manager → both the gateway's `ESCUREL_WEBHOOK_SECRET` and the
runner's `ESCUREL_WEBHOOK_SECRET`).

### Why HMAC over the body, not a bearer header

A static bearer/shared-secret header authenticates the *caller* but not
the *message*: an intermediary can keep the header and rewrite the event.
HMAC over the exact body bytes binds the secret to the content, so any
tampering invalidates the signature. It is the same scheme GitHub and
Stripe use for their webhooks (`X-Hub-Signature-256` /
`Stripe-Signature`), so the contract is familiar to integrators. We do
**not** add a timestamp/nonce anti-replay window in v1 — at-least-once
delivery with effectively-once processing (the run ledger's unique
`(tenant, event_id)` key, #149) already absorbs replays; a replay
produces no second run.

## Consequences

- **New config surface.** `ESCUREL_WEBHOOK_SECRET` on both processes
  (gateway threads it through `Config` → `ServerConfig` →
  `Webhook::new(url, secret)`; the runner already loads it via
  `RunnerConfig`). Unset → unsigned (dev). The webhook stays opt-in.
- **New dependency.** `hmac` + `sha2` (0.12 / 0.10, matching the
  versions already in the workspace) added to `escurel-server` and
  `escurel-runner`.
- **Payload shape grows by one field.** `tenant_id` is additive; the
  runner's `Event` deserialize (`#[serde(default)]`, no
  `deny_unknown_fields`) ignores it, and the tenant is read from the raw
  JSON. Existing consumers that ignore unknown fields are unaffected.
- **Trust model.** The webhook secret is the **only** ingress trust
  anchor between the gateway and the runner (per the contract's
  concurrency/safety model). It is not a substitute for network-level
  isolation (tailnet-only exposure on the substrate).
- **No anti-replay window.** Deliberately deferred to the ledger's
  idempotency; revisit if a deployment needs strict replay rejection.

## Open follow-ups

- Per-tenant routing. The gateway is single-tenant per `Indexer` today,
  so `indexer.tenant()` is unambiguous. If the gateway grows
  multi-tenant indexer routing, the authoritative tenant for the payload
  must come from the capture's auth context rather than a process-global
  indexer tenant.
- Optional timestamped anti-replay (`X-Escurel-Webhook-Timestamp` folded
  into the signed material) if a deployment needs replay rejection
  independent of the ledger.
