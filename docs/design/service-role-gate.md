# Service role gate

Status: implemented (skill 0.25.19; lineage read and `acl.reply_into` 0.25.21). Decision: a gateway-configured tool allow-list per service role, plus
skill-declared ACL grants. Neither half alone meets the requirement.

## Problem

An application runs an asynchronous worker under a service identity: `sub: agent:<name>`, one dedicated role value in
the roles claim (e.g. `app:worker`), `act.sub` naming the control service that issued the token. The worker must

- read every event of one request label in its tenant, including objectless events captured by other people, and only
  the attachment pages those events reference;
- propose review drafts for a declared set of skills, without write rights on them;
- capture replies to visible requests and assign visible requests;
- never promote or discard drafts, decide changesets, write pages, ingest or administer, even when it calls `/mcp`
  directly rather than through its own client allow-list.

Today a role group can only widen access: `create_draft` needs the target's write ACL, an unassigned event is readable
only by its capturer, `assign_event` needs write on the target, and `promote_draft`/`discard_draft` are agent-scope
tools gated by data ACLs, not by the caller's role. Data grants cannot express "this caller may call fewer tools".

## Options

| | (a) gateway config: role → allowed tools | (b) skill ACL grants | chosen |
|---|---|---|---|
| denies promote/discard/changeset decisions/admin | yes, deny-by-default | no (grants only widen) | (a) |
| event visibility, objectless included | no | `acl.read_events` on the event skill | (b) |
| drafts without write rights, per skill | no | `acl.propose` on the target skill | (b) |
| attachments referenced by visible events only | no | `acl.read_referenced` on the attachment skill | (b) |

The tool list is an operator decision about a credential, so it lives with the other auth configuration (env), where a
tenant admin editing skill pages cannot widen it. Which skills a role may read or propose is a data-model decision, so it
lives in the skill pages next to `acl.read/create/update`. Both are keyed on the same role value, which the ACL already
treats as a group name.

## Design

**Tool gate.** `ESCUREL_AUTH_ROLE_TOOLS="app:worker=list_inbox,list_events,expand,...;app:other=..."`. A token whose
groups contain a configured role value is restricted: a tool outside the allow-list is refused before dispatch with
`-32001` and `data.code: "role_restricted"`, and `tools/list` shows only the allowed tools. A token holding two
configured roles gets the intersection. Boot fails on an unknown tool name, an admin- or operator-scope tool, a role
value in the reserved `escurel:` namespace (minters strip it, so a child token would lose the restriction) or equal to
the admin/operator value. A restricted token is also refused at `/ingest`, `/ingest/upload`, `/blob` and `/ws`: the
gate is deny-by-default, and those surfaces are not tools.

Two allowed tools behave more narrowly for a restricted caller:

- `create_draft` requires an `acl.propose` grant on the target skill, whatever the write-ACL mode and whatever write
  grants the role also holds. Non-instance targets (skill pages) are refused. The draft is still validated.
- `capture_event` requires a lineage root (`provenance.runner.root_event_id`) that the caller may read, and either the
  root's `label_skill` or a label whose skill grants the caller `acl.reply_into`: a restricted caller files replies,
  never new work.

**Minted children.** `mint_agent_token` copies the caller's groups (minus `escurel:` values) into the child, so the
child carries the role value and the same gate. The mint is refused when the gateway reads groups from a claim other
than `roles` (the claim the minter writes), because the child would lose the restriction.

**`acl.propose: [groups]`** (target skill). Holders may `create_draft` an instance of this skill: the content's
`skill:` must match the target path, and an existing target must be an instance of the same skill. It is not a write
grant: `update_page` and `promote_draft` still check `create`/`update`.

**`acl.read_events: [groups]`** (event skill, i.e. the skill a `label_skill` names). Holders may read every event under
that label in the tenant, objectless or filed, under any event-ACL mode. An unassigned event visible through this grant
may be assigned to an instance page the caller can read; write on the target is not required. An unassigned event that
is visible only by the ordinary rules still needs write on the target.

**`acl.read_referenced: [groups]`** (attachment skill). Holders may read an instance of this skill when an event they
read through `acl.read_events` references it by `[[skill::id]]` in its body, and the event's capturer may read the
instance by the ordinary rules (their subject and stored group membership). The second condition stops a requester from
naming another person's attachment. The grant only widens: a role that already reads the skill through `read` (for
example `read: [public]`) reads every instance. To keep unreferenced attachments private from the role, narrow `read`
to the human groups, e.g. `read: [owner, editors]`.

**`acl.reply_into: [groups]`** (reply skill). A restricted holder may capture a reply under this skill's label into a
readable root of another label. Declared on the skill, not in `ESCUREL_AUTH_ROLE_TOOLS`: which labels a role may write
under is data, like what it may read or propose, and a skill author adding a reply type should not need an operator
change. The config stays a pure tool list, so one parser and one rule ("tools in config, data in skills") remain. It
does not widen an unrestricted caller, who may already capture under any label. Assigning the root is unchanged.

**Lineage read** (all callers, event ACL `log`/`enforce`). The subject that captured a lineage root reads every `user`
event in that lineage (`root_event_id`, set from `provenance.runner|review.root_event_id`), filed or not, whoever
captured it. A run's `system` bookkeeping under the root is not a reply and stays hidden.
Without it the requester of an objectless request could not read the service's unassigned reply, which is visible only
to its capturer. Only the root's capturer gains, and only within that lineage. This deliberately lets a reply filed into
a record the requester cannot read stay readable to the requester: the reply was addressed to them. Any caller can still
address a reply to any root id, which discloses that reply to the root's capturer and to nobody else.

Tokens without a configured role are unaffected, and skills without the new keys behave as before.

## Revocation limit

The gateway verifies self-contained JWTs. A disabled service credential stays usable at the gateway until its access
token expires; with the five-minute tokens the control services issue, that is the window. A worker that reaches the
gateway only through its application's proxy, which checks the credential on admission, closes it there. A minted child
lives until its own `ttl_secs`; keep that short.

## Falsifiers

- A worker that needs a tool the gate cannot express per call (for example `assign_event` into arbitrary pages) means
  the grant needs a target predicate, not a wider allow-list.
- If skill authors put a service role into `read: [public]`-style grants routinely, `read_referenced` should become a
  restriction (deny unless referenced) rather than a grant.
