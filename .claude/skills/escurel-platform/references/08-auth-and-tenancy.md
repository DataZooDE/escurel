# 08 — Auth and tenancy

Escurel is **per-tenant** and **OIDC-bearer** authenticated. Each server
instance is scoped to one tenant; your app authenticates every call with a
JWT and gets exactly that tenant's content. Canonical: `docs/spec/platform.md`
§auth; deploy binding: `docs/deploy/substrate.md` §1.

## The bearer + the claims

Every call carries `Authorization: Bearer <jwt>` (HTTP/MCP) or an
`authorization` gRPC metadata header. The server verifies it against the
issuer's JWKS and reads a small, **configurable** set of claims
(`platform.md` defaults / substrate deploy overrides):

| concept | claim | default | substrate value |
|---|---|---|---|
| audience | `aud` | `escurel` | `escurel` |
| tenant | `tenant_claim` | `tenant` | `escurel_tenant` |
| role list | `admin_role_claim` | `roles` | `roles` |
| admin grant | `admin_role_value` | `escurel:admin` | `escurel:admin` |

Verification flow (`platform.md`): extract bearer → verify signature
against cached JWKS → resolve `tenant_id` from the tenant claim → resolve
**role** (admin iff `admin_role_value` ∈ the role-claim array; otherwise
agent) → stamp `(tenant_id, role, sub)` onto the request. The stamped role
surfaces as `escurel.role = "agent" | "admin"`.

So: your app's token must carry the right audience, a tenant claim naming
the tenant, and — only for admin operations — the admin role value. A
mismatched tenant in a request body is rejected; an agent-role token never
sees the admin tools (they're not even listed in `tools/list`).

## Two roles

- **Agent** — the normal app surface: all fourteen agent tools
  (`references/02`), including `append_message` / `list_messages` for
  chat history.
- **Admin** (`escurel:admin`) — tenant CRUD + operator inspection
  (`admin_list_lanes`, `admin_lane_keys`, `admin_index_query`, …) plus
  the destructive purges: `tenant_delete` and `DeleteChatHistory` (chat
  retention + GDPR right-to-erasure). The agent role can never delete
  chat history — by design. Out of scope for a typical consuming app
  except where the app schedules its own retention cron; see
  `references/10`.

## In tests: `AuthMode`

`escurel-test-support` (`references/06`) gives you the whole OIDC dance as
one enum — no `wiremock`/`jsonwebtoken`/`rsa` in your test code:

- `AuthMode::Disabled` — `/mcp` is unauthenticated. Smoke tests only.
- `AuthMode::TestIssuer` — the process stands up an in-process JWKS
  endpoint with an ephemeral RSA keypair. **`mint_token(tenant, role)`**
  signs a JWT the running server accepts. This is the default choice for
  app integration tests.
- `AuthMode::External { issuer_url, jwks_url }` — point at a real OIDC to
  exercise the production auth path end-to-end.

```rust
let escurel = EscurelProcess::spawn(Opts { auth: AuthMode::TestIssuer, .. }).await;
let agent_tok = escurel.mint_token("acme", Role::Agent);
let admin_tok = escurel.mint_token("acme", Role::Admin);   // for admin-surface tests
```

`mint_token` panics under any mode other than `TestIssuer` — the support
crate has no business signing tokens for a real realm.

## In production

Your app obtains its bearer from the real issuer your deployment names
(on the substrate, a Vault OIDC role or a Dex/Keycloak instance —
`substrate-platform` + `docs/deploy/substrate.md` §1), then passes it to
`Client::connect(endpoint, SecretString::from(token))` (`references/05`)
or as the `Bearer` header (`references/03`). Wrap it in `SecretString`;
never log it. Tokens are short-lived — refresh on the issuer's schedule.

## Multi-tenant apps

One `EscurelProcess` (and one deployed gateway) is one tenant's scope. In
tests, mint tokens for different tenants and use `client_for(tenant, role)`
(`references/06`). `FixtureBuilder` can seed multiple tenants by chaining
`.tenant(...)…done().tenant(...)…done()`. Cross-tenant *operations* in a
single call are not supported (`references/10`).
