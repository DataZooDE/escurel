# ADR-0004 — Group/role-based per-instance ACL (group ACL v1)

**Status:** Accepted, 2026-06-18.
**Supersedes:** the binary `visibility: public|owner` read policy
introduced for per-instance access control (it becomes a special case of
this model). Backward-compatible — existing pages are unchanged.
**Scope:** the **data-level** ACL (read/write/chat over skill instances).
Capability/operator-tool RBAC and instance-level overrides are **phase 2**
(designed, not implemented).

## Context

escurel's access control was **two global roles** (`Agent | Admin`,
[`escurel-auth`](../../crates/escurel-auth/src/verifier.rs)) plus
**per-instance ownership**
([`escurel-index/src/acl.rs`](../../crates/escurel-index/src/acl.rs)): a
skill page declared `visibility: public | owner` and an `owner_field:`,
and the read/write decision was a deterministic, fail-closed comparison
of the resolved owner against the token `sub`. There were no groups, no
custom roles, and no read/write/delete matrix beyond the read-vs-write
asymmetry; admin was all-or-nothing per tenant. Sharing an instance
beyond its single owner, or letting a role like `moderator` act across
owners, was impossible without making the data world-readable.

## Decision

Replace the binary visibility with a **name-based group model**: each
skill declares, per CRUD verb, a list of **group names** that may perform
that verb. The decision stays deterministic and fail-closed — **no LLM,
ever**.

```yaml
owner_field: author
acl:
  read:   [public]
  create: [owner]
  update: [owner, moderator]
  delete: [admin]
```

1. **One primitive.** A "group" is a flat per-tenant name. "Role" and
   "group" are the same thing (D4).
2. **Reserved groups** `public` / `owner` / `admin` are resolved
   *structurally* — always-present-for-authenticated / structural-owner /
   verified-admin. They are **never** grantable via a token claim or a
   membership row (stripped before the intersection), so a misconfigured
   IdP can never impersonate them (R2/§8.2). `admin` derives only from
   `admin_role_value`, never the literal string.
3. **Custom groups** are satisfied from **either** the JWT `groups_claim`
   **or** the DuckDB-canonical `group_members` table (D6/D11). A
   token-only group needs no escurel state.
4. **Decision:** `allowed ⇔ effective_groups ∩ acl.<verb> ≠ ∅`, or the
   caller is admin (implicit bypass, D9). Pure allow-list union; no deny
   rules in v1.
5. **Resolution order** for `(skill, verb)` (§4.5): skill `acl.<verb>` →
   legacy `visibility:` mapping → tenant `acl_defaults` (on the `escurel`
   meta-skill page) → shipped default → fail-closed. The shipped default
   (`read:[public]`, writes `[admin]`) reproduces today's behaviour, so an
   unconfigured tenant and any un-migrated `visibility:` page authorise
   **bit-identically**.
6. **Membership store:** a first-class `group_members` table (current-
   state only, with `added_at`/`added_by` audit), created via an
   idempotent every-boot ensure-step. Mutated only by the **admin-only**
   `add_group_member` / `remove_group_member` / `list_group_members` MCP
   tools (D14).
7. **Groups claim:** `groups_claim` (default `roles`) is parsed leniently
   — a JSON array, or a single string split on whitespace/commas (R1).

## Decisions resolved with the stakeholder (2026-06-18)

These four refine the change request against the real codebase:

- **Schema rollout** — the table is added via an idempotent every-boot
  `CREATE TABLE IF NOT EXISTS` ensure-step, not a new schema-version
  framework. The v1 schema's `Migrator::up` runs only on a fresh DB, so a
  per-boot ensure is what reaches already-provisioned tenant DBs.
- **`admin_role_value` stripped from token groups** — beyond the three
  reserved names, the configured admin value (e.g. `escurel:admin`) is
  also stripped from `token_groups` at the server boundary, so it can
  never act as a phantom custom group. A mild extension of §8.2 (which
  strips only the three literals), for hygiene.
- **Tool visibility** — the new admin tools are listed for everyone and
  gated at dispatch (`require_admin` → JSON-RPC `-32001`), exactly like
  every existing admin tool. The change request's claim (§6) that admin
  tools are already hidden from `tools/list` is **inaccurate**; there is
  no such hiding to mirror, and adding it would change behaviour for all
  admin tools. Not done in v1.
- **Demo app in scope** — the membership tools are wired into
  `apps/escurel-explore` and `scripts/verify-demo.sh`, per the project
  contract that the demo tracks every backend capability.

## Consequences

- **Backward-compatible.** Proven by the unchanged `instance_acl` /
  `chat_acl` / `write_acl` / `stored_query_acl` suites plus a
  legacy-equivalence matrix test.
- **`delete` is enforced as `update` in v1.** There is no distinct delete
  operation at the write boundary (deletion is a tombstone overwrite), so
  the `delete:` list is parsed and reported but enforced through the
  `update` path. A distinct delete gate is phase 2.
- **Membership is not time-travelled.** `group_members` is current-state
  relational data, deliberately outside the CRDT/markdown lane (R3) —
  high-churn, joined in SQL on the read path, no history bloat.
- **Chat stays compat.** `may_access_chat` keeps its owner-or-ungated
  behaviour (R4): a chat group with no owning member instance, or an
  unresolvable owner, stays open.
- **Phase 2** (not implemented): capability/operator-tool RBAC
  (`require_capability`), role hierarchy, and instance-level `acl:`
  overrides (the instance-frontmatter `acl:` key is reserved and ignored
  in v1, R5).

## Code map

- `escurel-auth/src/verifier.rs` — `groups` on `AuthContext`,
  `groups_claim` on `OidcConfig`, `parse_groups_claim`.
- `escurel-index/src/read.rs` — `AclPolicy` + `acl` on `SkillInfo`,
  `parse_named_acl`, legacy mapping.
- `escurel-index/src/acl.rs` — `token_groups` on `AclCaller`,
  `caller_groups` / `resolve_policy` / `tenant_acl_defaults`, per-verb
  decision core.
- `escurel-index/src/groups.rs` + `sql/0005_group_members.sql` +
  `schema.rs::ensure_group_members` — membership store.
- `escurel-server/src/mcp.rs` — caller construction, the three membership
  tools, `list_skills` `acl` projection.
- `escurel-types/src/agent.rs` — `SkillAcl` + `Skill.acl`.
