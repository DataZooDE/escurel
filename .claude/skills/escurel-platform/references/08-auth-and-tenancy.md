# 08 — Auth and tenancy

Escurel is **per-tenant** and **OIDC-bearer** authenticated. Each server
instance is scoped to one tenant; your app authenticates every call with a
JWT and gets exactly that tenant's content. Canonical: `docs/spec/platform.md`
§auth; deploy binding: `docs/deploy/substrate.md` §1.

## The bearer + the claims

Every call carries `Authorization: Bearer <jwt>` (HTTP/MCP). The server
verifies it against the
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

A **per-run agent token** (the bearer the runner mints for one harness run,
`sub: agent:<skill>`, `act.sub: escurel-runner`) additionally carries the
run's identity as claims — `run_id`, `root_event_id`, `trace_id` (optional).
The gateway reads them to stamp lineage onto what the run writes (a draft's
`run_id` / `root_event_id`) and to authorise `report_progress` for exactly
that run. They are never an authorization input for anything else, and an
ordinary bearer simply has none. Only a runner in **minted** mode carries
them; a dev runner on a pasted `ESCUREL_RUNNER_TOKEN` cannot mint, so its
runs write unstamped.

Its **authority** is the runner's own — `roles: [escurel:admin]` — by
default. With `ESCUREL_RUNNER_AGENT_NARROW=1` on the runner it is instead
NARROWED to the target skill: `roles: [escurel:agent, <the skill's
acl.create ∪ acl.update groups>]`, so under `ESCUREL_WRITE_ACL=enforce`
the harness may write that skill's instances and nothing else, while the
runner keeps admin for its own bookkeeping (run events, cascades). The
groups come from the skill page, so — as for a run board — every
`escurel:`-prefixed name and every reserved structural group (`public`,
`owner`, `admin`) is stripped before signing: a skill page can grant its
agent an engagement group, never a privileged role. A skill that declares
no write grant runs an agent that can write nothing (the tenant default is
admin-only), which is why the flag ships off until a corpus's write-ACL
model is in place. Note that `assign_event` is not write-ACL gated, so a
narrowed agent whose write was refused can still mark the event processed;
the echo harness stops on a refused write, and a real skill should too.
Groups are tenant-wide, so a narrowed token also carries a `skill` claim
(the target skill's id) and the write ACL refuses an instance write under
any other skill before it consults the groups — a token narrowed to
`renewal` cannot write `billing` even when both grant `ops`. It is the one
claim on the token that is an authorization input, and only ever a
restriction.

A **workbench agent token** is the same shape minted by the GATEWAY
(`mint_agent_token`) for an interactive agent with no runner behind it:
`sub: agent:<skill>`, `act.sub: <the human who asked>`,
`purpose: workbench_agent`, the run claims, and the human's own authority —
an admin's mint carries `escurel:admin`, a member's carries their groups
with every reserved `escurel:` role stripped. The gateway authorises it AS
THE HUMAN: the ACL subject is `act.sub`, the agent is the actor, so what
the agent writes reads `captured_by: <human>` / `captured_via:
agent:<skill>`, and an instance owned by the agent principal confers
nothing on the human who minted it. It needs the gateway to have
a signing identity: `ESCUREL_AUTH_SIGNING_KEY` (an RSA private key some
trusted issuer's JWKS publishes), `ESCUREL_AUTH_SIGNING_KID`,
`ESCUREL_AUTH_SIGNING_ISSUER` (defaults to the OIDC issuer). Without it
the tool answers `unsupported`.

So: your app's token must carry the right audience, a tenant claim naming
the tenant, and — only for admin operations — the admin role value. A
mismatched tenant in a request body is rejected. `tools/list` is
role-scoped: an agent-role token receives only the `scope: "agent"`
subset; admin tools are listed for admin tokens and refused at dispatch
(`-32001`) regardless.

## Two roles

- **Agent** — the normal app surface: the ~29 agent tools
  (`references/02`), including `append_message` / `list_messages` for
  chat history and the event-bus quartet.
- **Admin** (`escurel:admin`) — tenant CRUD + operator inspection
  (`admin_list_lanes`, `admin_lane_keys`, `admin_index_query`, …) plus
  the destructive purges: `tenant_delete`, `purge_page` and
  `admin_delete_chat_history` (chat
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
(on the substrate, the shared OIDC root — Triton / Carl / the explore BFF;
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
