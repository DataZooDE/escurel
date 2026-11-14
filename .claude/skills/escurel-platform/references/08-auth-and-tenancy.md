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
| operator grant | `ESCUREL_AUTH_OPERATOR_ROLE_VALUE` | `escurel:operator` | `escurel:operator` |
| runner service role | `ESCUREL_AUTH_RUNNER_ROLE_VALUE` | `escurel:runner` | `escurel:runner` |
| bridge service role | `ESCUREL_AUTH_BRIDGE_ROLE_VALUE` | `escurel:bridge` | `escurel:bridge` |
| groups | `ESCUREL_AUTH_GROUPS_CLAIM` | `roles` | `roles` |

A caller's ACL groups are the token's groups (that claim, reserved names removed) **plus** their `group_members`
rows for the token's `sub` (the admin group tools). A chat run's token carries
no groups of its own, so for those only `group_members` counts; manage group grants there when a run must use them.

Verification flow (`platform.md`): extract bearer → verify signature
against cached JWKS → resolve `tenant_id` from the tenant claim → resolve
**role** (admin iff `admin_role_value` ∈ the role-claim array; otherwise
agent) → stamp `(tenant_id, role, sub)` onto the request. The stamped role
surfaces as `escurel.role = "agent" | "admin"`.

**Operator.** `escurel:operator` in the same role claim marks an operator, which is also an admin. A tenant
admin may act only on its own tenant: an admin tool whose `tenant_id` names another tenant is refused (`-32002`). The
operator is the only credential that may name another tenant, and only in the tools that work on the tenant store
(`tenant_export`, `tenant_import`); tools bound to this gateway's own index or lanes (`compact_lanes`, `rebuild`,
`admin_quota`, `admin_audit`, ...) refuse a foreign tenant for everyone. The tenant registry (`tenant_create`,
`tenant_list`, `tenant_get`, `tenant_update`, `tenant_delete`) needs an operator (`-32001`, `operator_required`). The
operator token's `tenant` claim is still the served tenant. Nothing the gateway or runner mints carries the operator
value. Tests: `process.mint_operator_token("acme")`; `escurel-test-gateway` prints an `operator_bearer`. Design:
`docs/design/operator-credential.md`.

**Service roles** (`ESCUREL_AUTH_ROLE_TOOLS="app:worker=list_inbox,list_events,expand,create_draft,...;<role>=..."`).
A token whose groups carry a listed role value may call only that role's tools; every other tool is refused with
`-32001`, `data.code: "role_restricted"`, and `tools/list` shows only the allowed ones. Two listed roles give the
intersection. A restricted token is also refused (`403 role_restricted`) at `/ingest`, `/ingest/upload`, `/blob` and
`/ws`. Only agent-scope tools may be listed; the gateway refuses to boot on an unknown or admin tool, a role in the
`escurel:` namespace, or the admin/operator value. Two allowed tools are narrower for a restricted caller: `create_draft`
needs an `acl.propose` grant on the target skill (write grants do not count), and `capture_event` must reply to an event
the caller can read (`provenance.runner.root_event_id`), under the root's `label_skill` or a label whose skill grants the
role `acl.reply_into`. `mint_agent_token` children carry the
parent's groups and so the same gate; it refuses for a restricted caller when `ESCUREL_AUTH_GROUPS_CLAIM` is not
`roles`. What the role may read or propose is declared on the skills (`acl.read_events`, `acl.read_referenced`,
`acl.propose`, references/01). Revocation: the gateway verifies self-contained JWTs, so a disabled service credential
works at a direct gateway until its access token expires; a minted child lives until its own `ttl_secs` (default 1800,
max 14400, no renewal). Design: `docs/design/service-role-gate.md`.

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
acl.create ∪ acl.update groups>]`, so under `ESCUREL_WRITE_ACL=enforce` (the default)
the harness may write that skill's instances and nothing else, while the
runner keeps admin for its own bookkeeping (run events, cascades). The
groups come from the skill page, so — as for a run board — every
`escurel:`-prefixed name and every reserved structural group (`public`,
`owner`, `admin`) is stripped before signing: a skill page can grant its
agent an engagement group, never a privileged role. A skill that declares
no write grant runs an agent that can write nothing (the tenant default is
admin-only), which is why the flag ships off until a corpus's write-ACL
model is in place. Note that `assign_event` checks the write ACL only for an
UNASSIGNED event, so a narrowed agent whose write was refused can still mark
an already-filed event processed (an agent profile's `scopes.write` closes
this for agent runs); the echo harness stops on a refused write, and a real
skill should too.
Groups are tenant-wide, so a narrowed token also carries a `skill` claim
(the target skill's id) and the write ACL refuses an instance write under
any other skill before it consults the groups — a token narrowed to
`renewal` cannot write `billing` even when both grant `ops`. It is the one
claim on the token that is an authorization input, and only ever a
restriction.

An **agent-profile token** (#823) is what the runner mints when a run has an
agent (references/01, *Agent profiles*): `sub: agent:<agent id>`, `act.sub:
escurel-runner`, the run claims, `roles: [escurel:agent, <the skill's write
groups>]` — **never `escurel:admin`, whatever `ESCUREL_RUNNER_AGENT_NARROW`
says** — the `skill` claim, and an **`agent` claim** naming the agent. The
token carries the agent's NAME only; the gateway reads the agent page itself
on every `/mcp` call, so a runner (or a forged token) cannot widen what the
page grants, and an admin's edit narrows the very next call:

- the agent page is missing, archived or not an `agent` instance → **every
  tool is refused**, JSON-RPC `-32001`, `data.code: agent_unknown`;
- `scopes.tools` present and the tool not in it → `-32001`,
  `data.code: agent_scope_denied` (checked with the role gate; it can only
  remove tools);
- `scopes.write` present and the page id matches no glob → the write is
  refused `{ok: false, issues: [{code: agent_scope_denied}]}` — on
  `update_page`, and as a refusal on every other write path (sessions,
  drafts, move/delete, promotion), plus `capture_event` / `assign_event`
  into a page outside the scope (`agent_scope_denied`). This binds even an
  admin-role token, and an agent token is always write-ACL **enforced**,
  whatever `ESCUREL_WRITE_ACL` says.
- `tools/list` on an agent token lists only the tools its `scopes.tools`
  admits (intersected with the role gate), read from the agent page the same
  way — so a harness is never offered a tool it would be refused; a token
  naming no agent page gets `agent_unknown` there too.

A refused call lands on the run's record: `get_run_tool_calls` shows it
with `status: rejected` (or `error`) and `error_code: agent_scope_denied`.
Agent tokens are served on `/mcp` only — `/ws`, `/ingest` and `/blob`
refuse them (`403`, `error: agent_scope_denied`), since only `/mcp` applies
the scopes. Agent pages themselves are admin-write-only (references/01) **in every
`ESCUREL_WRITE_ACL` mode**: `off` and `log` relax the ordinary write ACL for a
rollout, but a write that touches the `agent` system skill — its skill page,
a page claiming it, anything under `markdown/instances/agent/` — is always
enforced (fail closed), on every write path (update, delete, move, drafts,
sessions).

A **workbench agent token** is the same shape minted by the GATEWAY
(`mint_agent_token`) for an interactive agent with no runner behind it:
`sub: agent:<skill>`, `act.sub: <the human who asked>`,
`purpose: workbench_agent`, the run claims, and the human's own groups —
never admin, whoever minted it (an admin who wants admin authority uses their
own token), with every reserved `escurel:` role stripped. Only a PERSON's
token mints: an agent-profile, narrowed or run-bound token calling
`mint_agent_token` is refused `-32001`, `data.code: agent_scope_denied` (the
child would carry the groups but shed the agent's scopes and skill
confinement). The gateway authorises it AS
THE HUMAN: the ACL subject is `act.sub`, the agent is the actor, so what
the agent writes reads `captured_by: <human>` / `captured_via:
agent:<skill>`, and an instance owned by the agent principal confers
nothing on the human who minted it. It needs the gateway to have
a signing identity: `ESCUREL_AUTH_SIGNING_KEY` (an RSA private key some
trusted issuer's JWKS publishes), `ESCUREL_AUTH_SIGNING_KID`,
`ESCUREL_AUTH_SIGNING_ISSUER` (defaults to the OIDC issuer). Without it
the tool answers `unsupported`.

**A chat bridge's token is restricted by its trust entry.** `escurel-chat-bridge` signs
a token for each person it answers with a key of its own, never the gateway's
main signing key. The bridge publishes the key's public half at `GET /.well-known/jwks.json`, which
the gateway trusts as an extra issuer (`ESCUREL_AUTH_OIDC_ISSUER_<N>` +
`ESCUREL_AUTH_JWKS_URI_<N>`, over https) and names as the bridge in `ESCUREL_AUTH_BRIDGE_ISSUER`,
pinned to the key's id in `ESCUREL_AUTH_BRIDGE_KID`. EVERY token from that issuer is a bridge
token whichever kid it names, and one under an unlisted kid is refused (`401`) — the bridge serves
that JWKS, so a kid-only restriction would not restrict whoever controls it. A bridge token
is bounded whatever it claims: at most 900 s (`exp - iat`; `exp - now` allows 60 s for a
bridge clock running ahead, and the bridge mints 840 s), never
admin or operator, **no roles or groups** (so ACLs that grant by IdP group do not
reach a person through the bridge — only those naming them, their
`group_members` groups, or `public`), refused (`401`) if it carries an
`agent`/`skill`/`run_id`/`root_event_id`/`purpose`/`act` claim or names no valid
`bridge_channel`. Its actor is `bridge:<channel>`, so what it captures reads
`captured_by: <person>` / `captured_via: bridge:msteams`; the chat rules accept
that via and no other. The bridge entry is pinned to one tenant
(`ESCUREL_AUTH_BRIDGE_TENANT`, required, = the served tenant): a bridge token
naming another is refused (`401`).

**A bridge token may call only what a chat turn needs** — it is a relay, not the
person on the agent surface:

| tool | only when |
|---|---|
| `whoami` | always |
| `update_page` | `base_sha256: ""` (create-if-absent) of an instance of a `chat: {page_bound: true}` skill |
| `append_message` | a `page-chat:` group, `role: user` (the answer is the run's) |
| `list_messages` | a `page-chat:` group |
| `capture_event` | `label_skill` = `ESCUREL_AUTH_BRIDGE_CHAT_SKILL` (default `chat`) |
| `ingest_upload` | always (attachments, as the person) |

Everything else — `expand`, `search`, any edit, any other label — is `-32003`
(`data.code: forbidden`), and `tools/list` offers only these six. `/ingest`,
`/blob` and `/ws` answer `403`. The ordinary ACLs still apply on top.

**A token minted for an agent is never admin or operator.** A token carrying
an `agent` or `skill` claim, or `purpose: workbench_agent`, projects the
agent role whatever its roles list — including a deployment's own
`ESCUREL_AUTH_ADMIN_ROLE_VALUE` / `ESCUREL_AUTH_OPERATOR_ROLE_VALUE` when
those name an IdP group: a skill page (member-writable) names its agent's
groups, so honouring the mapping there would let a page author mint an
admin agent.

**Steering a run (`inject`).** A machine caller (any agent-profile, narrowed
or run-bound token, admin role or not) is refused (`agent_scope_denied`) in
every mode: a run's own token never directs a run. A person passes the same
gate as `cancel` — the run's target page's write ACL, in the deployment's
`ESCUREL_WRITE_ACL` mode. The bound is the page writer, deliberately: whoever
may write the page could already capture an event whose body tells a run on
it anything, so `inject` grants no new authority — it only saves the cancel
and re-trigger. Under `off` / `log` that is every caller, as it is for every
other write. `inject` does reach a run whose token is the runner's admin; it
changes what the run is told, not what the run may do.

**Runners outside the cluster (#818).** A runner on a laptop or an on-prem
host does not get the platform's signing key. The operator issues it its
OWN credential instead: a bearer for a service subject (`runner:laptop-jr`)
with `roles: [escurel:admin]` and no run, agent or actor claims, set as
`ESCUREL_RUNNER_TOKEN` — or, for a short-lived credential a sidecar
refreshes, written to a file named by `ESCUREL_RUNNER_TOKEN_FILE`, which the
runner re-reads whenever it changes. The subject must not contain `#` (the
lease holder is `<sub>#<holder_id>`; such a subject cannot claim). A runner
whose credential is not an admin takes no inbox work: its claims
are refused, and it logs that the credential needs `escurel:admin`. That identity is not a machine caller, so it may do
the runner's bookkeeping — `report_runner_status`, write its `runtime` page
(admin-only), `claim_event` / `renew_claim` — and it may call `mint_agent_token` for a per-run
child. The child is what `mint_agent_token` always returns: never admin, the
caller's groups, `act.sub` the runner, a gateway-assigned `run_id`; and as a
machine caller it can neither mint again nor claim. No exception to the
machine-caller rule was needed. The claim rule is exactly that: any
**non-machine admin identity** may claim — a runner's admin bearer, and also a
person's admin token (a non-admin token is refused `-32001`, a run or agent
token `-32602` `forbidden`, a bridge token `-32003` before it gets that far). An admin who claims can park an event away
from every runner for up to an hour, which is no more than an admin can do to
the event directly. The lease's holder is `<sub>#<holder_id>`: `holder_id` is
the caller's own label for replicas sharing one credential, not an identity,
so a caller can never renew or take another identity's lease by naming its
`holder_id`. Today the runner itself mints run tokens
only in **minted** mode (it holds a signing key); on an operator-issued
static credential its runs use that credential as before (chat runs are
refused, see references/11), so give such a runner work that its own
authority should do, and scope it with the credential's groups.

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

## Built-in service roles (#828)

Two role values in the same claim as admin mark a **service's own
credential**, each with a narrow authority of its own. Unlike
`ESCUREL_AUTH_ROLE_TOOLS` (a ceiling that can only take tools away), these
*grant*:

- **`escurel:runner`** — what a runner needs and nothing more. It reads every
  inbox event (to see what it may claim), calls `claim_event` / `renew_claim`
  / `mint_run_token` (admin-scope tools, granted to this role by name), files
  the runner's own records (`run:` / `sched:` / `lint-` ids, `kind: system`,
  `escurel:` labels, `report_runner_status`, `put_run_transcript`) and writes
  its own `runtime` instance — it creates one, or updates one it last wrote;
  another subject's runtime page (another runner's, or one an admin edited) is
  refused, so one runner cannot rewrite another's harnesses or labels. Replicas
  sharing one credential share its pages. Everything else follows its ordinary ACL: no
  other page write, no admin tool, `list_runtimes` without the admin detail.
  `whoami` answers `service: "runner"`. An out-of-cluster runner holds only
  this and **no signing key**: the gateway mints each run's token
  (`mint_run_token`, references/02, references/11). **What that reaches:** a
  run token carries the run skill's `acl.create` / `acl.update` groups (and,
  as those groups, whatever they may read tenant-wide), so the gateway mints
  one only for a run whose `run-started` the same credential filed, of that
  very skill, and — for an agent run — only when the agent page lists the
  skill. A runner credential therefore reaches the skills it actually runs,
  never another skill's groups by naming it. It also **reads every event
  body** in the tenant, including ACL-restricted ones (chat, HR): it must, to
  decide what to admit. Treat it as a credential with tenant-wide read on the
  event bus.
- **`escurel:bridge`** — a chat bridge's service credential (`whoami`
  `service: "bridge"`); its minting tool arrives with the bridge work.

Both are ignored on a **machine** token (one carrying `run_id`, `skill`,
`agent` or `act`): a run token a runner minted never inherits the runner
role. Neither is admin, neither is a group name (stripped from the RBAC
groups like the admin value), and the four built-in values must differ.

## External agent tokens and revocation (#829)

An admin issues a bearer for an agent that runs **outside** escurel
(`issue_agent_token`, `escurel agent token issue <agent>`): `sub:
agent:<id>`, the admin as `act.sub`, `escurel:agent` plus the write groups of
the agent's skills, the `agent` claim. The gateway enforces the agent page's
scopes on every call, exactly as for a run (references/12).

**Revocation is an epoch on the agent page.** `token_epoch: <unix seconds>`
refuses every token for that agent issued before it — external tokens and run
tokens alike — with `-32001` `agent_token_revoked`, from the next call on (the
page is read per call). A token with an `agent` claim and no `iat` is refused
the same way. `revoke_agent_tokens` (`escurel agent token revoke <agent>`)
sets the epoch on the **gateway's** clock to `max(now + 1, current + 1)`:
strictly increasing, so a client's clock never decides it and revoke → issue
→ revoke inside one second still refuses the token issued in between. A token
issued (or a run token minted) after the revoke carries an `iat` raised to the
epoch, so it stands. **A revoke ends in-flight runs' tokens too:** the attempt
under way fails on its next call, and a retry — when the run has attempts left
— runs on a fresh token (a keyless runner asks the gateway for a new token on
every attempt). Only an admin may write an agent page.

## Skill authority keys

Some skill-page keys are **authority**, not description: `acl`, `visibility`,
`owner_field`, `instance_acl`, `agent`, `autonomy`, `schedule`,
`schedule_budget`, `schedule_webhook`, `requires`, `harness`, `model`, `cascade`, `actions` (its
`kind: event` entries trigger cascades), `chat`, `backend` (a `kind:
workflow` backend is a trigger too), `promotable`, `protected`, `immutable`,
`freeze_when`. `layer` has its own guard; descriptive keys are free.
The list is one constant, `escurel_types::skill_authority::AUTHORITY_KEYS`;
the SKILL.md importer names the same keys (references/04).

A write that **adds, removes or changes** any of them on a skill page lands
only for:

- an `escurel:admin` person, or
- a person in the **stored** skill page's `acl.update` groups (token groups or
  `add_group_member` membership; the structural `public` group grants
  nothing here). The incoming page's `acl:` is never consulted, so a draft
  cannot add its author and promote itself.

Members of the stored `acl.update` may change `agent` and `autonomy` too, by
design: an agent serves only the skills its **admin-only** profile lists
(`skills:` on the `agent` page, references/01), so binding an agent that does
not list the skill gets the skill nothing of that agent's — the run is
refused, never run as that agent nor on a wider token. Which skills an agent
may run as is the admin's decision, made on the agent page.

A **new** skill page carrying any authority key, and a stored skill without
`acl.update`, are therefore an admin's. A machine caller (run, narrowed
agent, delegated `act` token) never changes them, admin role or not — its
skill-page edits are held as drafts anyway. `description`, `summary`,
`fields`, the body and every other key stay free under the ordinary write rule.

A page is a skill page when its path is under `markdown/skills/` or its
frontmatter says `kind: skill`. Relabelling counts: a page that **becomes** a
skill (an instance rewritten as `kind: skill`) is judged as a create, so every
authority key it carries needs an admin even if the instance carried the same
values as data; a page that **stops** being a skill removes all its keys.

Every write door applies it: `update_page` (branch writes included),
`promote_draft` and `promote_changeset` (the **promoter** is judged, not the
draft's author; a changeset refuses before any member lands), the
`close_session` commit, `delete_page` (deleting a skill removes its ACL) and
`move_page` (the source loses its keys; the destination is a create), and
`merge_branch` (refused before any page lands). It judges what would
**persist** — after a stale `base_version` auto-merge, the merged bytes, not
the bytes you sent — against the page as stored when the write runs, so a
write based on an older head cannot revert an admin's newer change.
`import_pack`, `rebase_pack` and `unsubscribe_pack` replace skill pages
wholesale, so they need a **person** with `escurel:admin`: a machine caller
is refused (`-32003`, `data.code: forbidden`) whatever its role.

The refusal is a JSON-RPC error `-32003` with `data: {code:
"authority_keys_need_admin", keys: [...]}` and a message naming the keys.
It follows `ESCUREL_WRITE_ACL`: `enforce` refuses, `log` logs the would-be
refusal and allows, `off` keeps the ungated behaviour. The `agent` and
`runtime` system skills stay admin-only in every mode (references/01).

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
