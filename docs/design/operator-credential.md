# Operator credential

Status: implemented (skill 0.25.10). Decision: cross-tenant administration is operator-only.

## Problem

`Role` has `Agent` and `Admin`. Admin doubled as operator, so a tenant admin could name another tenant in
`tenant_export`, `tenant_import` and the tenant registry tools. The gateway serves one tenant and refuses a token whose
`tenant` claim differs, but the `tenant_id` argument of an admin tool is a second, unchecked path.

## Design

- A token is an operator iff its roles claim (`admin_role_claim`, default `roles`) contains `operator_role_value`
  (`ESCUREL_AUTH_OPERATOR_ROLE_VALUE`, default `escurel:operator`). Additive: no new claim name, no new config
  required, existing tokens unchanged.
- `AuthContext.operator: bool`. `Role` is unchanged: an operator also has `Role::Admin`, so every admin gate passes. No
  verifier (open dev gateway) counts as operator, as it counts as admin.
- Minters never emit it. Every `escurel:`-prefixed value is already stripped from group lists, and the operator value is
  also removed from RBAC groups (`auth_gate::rbac_groups`). Operators come from the identity provider.
- The `tenant` claim of an operator token is still checked at the gate: it names the served tenant (the control-plane
  instance). The operator reaches another tenant through the `tenant_id` argument, not through the claim.

## Rules (one helper: `ensure_tenant_access`, `mcp/tools_admin.rs`)

| tool | tenant admin, own | tenant admin, foreign | operator, foreign |
|---|---|---|---|
| `tenant_export`, `tenant_import` (store) | ok | refused `-32002` | ok |
| `compact_lanes` (gateway lanes) | ok | refused `-32002` | refused `-32002` ("served by another gateway") |
| `admin_quota`, `admin_audit`, `rebuild`, `attach_external`, `admin_purge_events`, ... (gateway index) | ok | refused `-32002` | refused `-32002` |
| `tenant_create/list/get/update/delete` | refused `-32001 operator_required` | refused | ok |

Why an operator is still refused on gateway-bound tools: the index and lanes behind this gateway belong to its served
tenant. Honouring a foreign `tenant_id` would act on the served tenant under another name. The operator calls the gateway
that serves that tenant. Only tools that read the tenant store directly can honestly cross tenants.

Falsifier: if a deployment runs several tenants per gateway, `TenantReach::Served` must become a lookup of the target
tenant's handles and the third column of the last three rows changes to ok.

## Operations

Grant the operator role value to a small set of identities in the IdP. Test gateways: `mint_operator_token(tenant)`;
`escurel-test-gateway` prints `operator_bearer` (also in `--bearer-file`).
