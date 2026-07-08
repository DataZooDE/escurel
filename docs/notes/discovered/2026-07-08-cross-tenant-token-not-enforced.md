# A validly-signed token for another tenant operated on the served tenant's data

**Symptom.** escurel binds one `Indexer` to one tenant (`ESCUREL_TENANT`), and
auth resolves a `tenant_id` from the token's `tenant` claim — but the two were
never compared. A token signed by the trusted issuer with the right audience
but a **different** `tenant` claim passed `enforce_auth`, had its quota debited
against *its* tenant, and then every read/write ran against the **served**
tenant's DuckDB (all data ops use `indexer.tenant()`, not the token's claim).
Net: any principal holding a valid token for tenant B could read and write
tenant A's corpus on A's instance. Only reachable when one issuer+audience
mints tokens for multiple tenants — i.e. exactly the multi-tenant setup.

The gap was masked because the only comparison (`ensure_tenant_matches`) guards
**admin** tools against their explicit `tenant_id` *argument*, not the token
claim on the agent/data path. A test (`auth_quota::tenants_have_independent_quota_state`)
even encoded the buggy behaviour — a `globex` token succeeding on the `acme`
gateway to "prove" per-tenant quota buckets.

**Recognise it by:** a request whose token `tenant` claim ≠ the gateway's
`ESCUREL_TENANT` returning `200`/`202` instead of a rejection; grep for
`enforce_auth` and check whether the served tenant is passed in.

**Fix.** `enforce_auth` (both the `mcp.rs` copy — used by `/mcp` + `/ingest` —
and the `ws.rs` copy for `/ws`) now takes the served tenant and rejects a
mismatch with **403 Forbidden** (`forbidden_tenant`), for **every** role incl.
admin (an operator uses a tenant-scoped token per instance). Skipped only when
no served tenant is configured (an unconfigured dev gateway, which also runs
without a verifier). This makes "one instance = one tenant" a real enforced
boundary — the substrate for the pet-per-tenant deployment model (see
`docs/spec/platform.md §Auth` step 4).

**Second pass — derive the served tenant from config, not the indexer.** The
first cut read the served tenant from `indexer.tenant()`, so `served == None`
whenever no indexer was wired. The admin tenant-CRUD tools
(`tenant_create`/`delete`/`export`/`import`) dispatch *ahead* of the indexer
gate off `tenant_store`, so a control-plane deployment with `verifier +
tenant_store + no indexer` skipped the tenant check and a foreign admin token
reached tenant-CRUD. Production always wires an indexer (`config.rs`), so this
was defence-in-depth, not a live breach — but the boundary should not hinge on
the indexer. `ServerConfig`/`AppState` now carry an explicit `served_tenant`
sourced from `ESCUREL_TENANT` (`serve()` falls back to `indexer.tenant()` for a
caller that only wires an indexer), and `enforce_auth` compares against that.
Regression test: `mcp_admin_tools::foreign_tenant_admin_forbidden_even_without_indexer`.

**Why not an in-process TenantManager.** Tenants are split graphs; escurel does
not federate across them (the client stitches). With few large tenants + the
remote embedding API there is no shared-model or evict-to-zero win, so the
boundary is enforced per-process rather than routed in-process. The
`TenantManager` design in `platform.md` is kept only as an escape hatch for a
future many-idle-tenants + local-model shape.
