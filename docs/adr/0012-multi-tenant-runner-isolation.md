# ADR-0012 — Multi-tenant runner isolation: per-tenant deploy, keys, and ledger

**Status:** Accepted, 2026-09-10.
**Builds on:** [ADR-0003](0003-capture-webhook-hmac-auth.md) (the `/trigger`
webhook HMAC), [ADR-0004](0004-rbac-groups.md) (group-based write ACLs), and the
gateway's hard one-instance-one-tenant boundary (`escurel-server`
`auth_gate::enforce_auth`).
**Scope:** the async-operations program (a fleet of `(escurel + agent + runner)`
stacks, one per customer). This ADR fixes the *isolation model* between tenants
for the runner; it does not add a facade (that is `start_operation`, a later
step).

## Context

The escurel **runner** (`escurel-runner`) is an autonomous process that mints
its own gateway bearer to drive workflow steps. The async-ops security review
raised the "single shared key = cross-tenant master key" risk: if every tenant's
runner signs with one shared key that every tenant's gateway trusts, whoever
holds that key can mint a token with any `tenant` claim and present it to that
tenant's gateway — a master key over the whole fleet.

Two facts about the existing architecture bound the problem:

1. **The gateway is one-instance-one-tenant.** `auth_gate::enforce_auth`
   verifies the bearer against the configured issuer(s)' JWKS (a `kid → key`
   lookup) and then refuses, with `403 forbidden`, any validly-signed token
   whose `tenant` claim is not the tenant this instance serves — for **every**
   role, admin included. A token minted for tenant B is useless against tenant
   A's gateway.
2. **The runner is one-process-one-tenant.** Its `tenant` is configuration
   (`ESCUREL_RUNNER_TENANT`), its signing key is one PEM, and its ledger is one
   file (`ESCUREL_RUNNER_LEDGER_PATH`).

So the isolation the review wants is **already enforceable** — it is a *deploy*
property, not new gateway code. What was missing was (a) a decision to deploy it
that way, (b) closing the `/trigger` body-tenant hole, and (c) this record so a
misconfiguration cannot silently reintroduce the master key.

## Decision

**One `(escurel-server + escurel-runner)` stack per tenant, each with its own
signing key, and each gateway trusting only its own tenant's issuer.**

1. **Per-tenant signing key, from GCP Secret Manager.** Each tenant's runner
   holds only that tenant's private signing key, fetched from GCP Secret
   Manager at deploy (no static keys in images or repo — the substrate's
   standing invariant). There is no shared runner key.

2. **Per-tenant gateway trust.** Each tenant's `escurel-server` is configured
   (`ESCUREL_AUTH_OIDC_ISSUER` / `ESCUREL_AUTH_JWKS_URI`, and the numbered
   `_2.._N` variants) to trust **only** the issuer/JWKS of *its* tenant's runner
   (plus the tenant's caller issuers). A gateway MUST NOT be configured to trust
   an issuer shared across tenants for the runner role. This, with fact (1)
   above, is what makes a per-tenant key an isolation boundary rather than
   decoration: tenant B's key is not in tenant A's trust set, and even a trusted
   key's token is refused unless its `tenant` claim matches (fact 1).

3. **`/trigger` tenant is the runner's own, never the body.** The runner takes
   its configured tenant as authoritative for an inbound `POST /trigger` and
   refuses (`403`) a body whose `tenant_id` names a different tenant — a party
   holding the webhook HMAC secret cannot drive another tenant's runs through
   this runner. (`resolve_trigger_tenant`, escurel-runner.)

4. **Per-tenant ledger file.** Each runner's SQLite ledger is its own file, so
   two tenants' runs can never share a row or a uniqueness key. This falls out
   of the per-tenant deployment (one `ESCUREL_RUNNER_LEDGER_PATH` per runner);
   no tenant-partitioned schema is needed.

## Consequences

- **Blast radius is one tenant.** A leaked runner key mints tokens only its own
  tenant's gateway trusts, and only for its own tenant's claim; it reads and
  writes nothing outside that tenant.
- **The load-bearing control is deploy configuration.** The escurel code already
  enforces the tenant-claim boundary and per-issuer trust; the isolation is only
  as strong as the deploy honouring (1) and (2). A deploy that points several
  tenants' gateways at one shared runner issuer silently rebuilds the master
  key. The substrate deploy owns a checklist asserting per-tenant issuers and
  per-tenant secrets, and should fail closed (refuse to render) on a shared
  runner issuer.
- **What this ADR does NOT cover.** It is about *which tenant* a run belongs to,
  not *what a run may do within its tenant*. A run today still executes under the
  runner's own (admin) identity, not the requesting caller's — the confused
  deputy. Fixing that (a per-run, caller-scoped token carrying the verified
  requester's subject + groups) requires the requester's groups to be captured
  server-side at invocation, which is `start_operation`'s job; it and the
  `capture_event` provenance/reserved-label sanitisation land with that facade.
- **Rotation.** A per-tenant key rotates by updating that tenant's Secret
  Manager secret and the gateway's trusted JWKS; no other tenant is touched.

## Substrate deploy requirements (checklist)

- [ ] Each tenant stack provisions a distinct runner signing key in GCP Secret
      Manager; the runner reads it at deploy, never from the image or repo.
- [ ] Each tenant's `escurel-server` trusts only its own tenant's runner issuer
      (no runner issuer shared across tenants).
- [ ] Each runner sets `ESCUREL_RUNNER_TENANT` and a per-tenant
      `ESCUREL_RUNNER_LEDGER_PATH`.
- [ ] The deploy fails closed if a runner issuer appears in more than one
      tenant's gateway trust set.
