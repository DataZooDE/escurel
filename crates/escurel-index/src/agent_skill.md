---
kind: skill
id: agent
description: An agent profile — the identity, harness, model and least-privilege
  scopes a runner's per-run token carries when it runs one of the listed skills.
required_frontmatter: []
optional_frontmatter: [harness, model, skills, scopes, max_concurrent, token_epoch]
instance_acl: false
acl:
  read: [public]
  create: [admin]
  update: [admin]
  delete: [admin]
---

# agent — who runs a skill, and with what authority

A system skill, shipped into every tenant. Only `escurel:admin` may create,
change or delete an agent page; the gateway refuses everyone else whatever
the tenant's ACL defaults say. An agent page is the authority a run carries,
so it is never something a run (or an ordinary member) can widen.

## Fields

- `harness` — the harness adapter this agent's runs use (`claude`, `codex`,
  `echo`, …). Must be in the runner's `ESCUREL_RUNNER_HARNESS_ALLOW`; a
  harness outside it refuses the run rather than running another one.
- `model` — the model id, within `ESCUREL_RUNNER_MODEL_ALLOW`.
- `skills` — the skills this agent may run, as wikilinks:
  `skills: ["[[skill::triage]]", "[[skill::note]]"]`. An event or a skill
  naming this agent is honoured only for a skill listed here.
- `scopes.tools` — when present, the only tools a run of this agent may
  call (`[search, expand, update_page]`). Absent = the role's usual set.
- `scopes.write` — when present, page-id globs the run may write
  (`["markdown/instances/note/*"]`; `*` stays inside one path segment,
  `**` crosses them). A write anywhere else is refused with
  `agent_scope_denied`. This narrows, never widens: the skill's own ACL and
  the token's skill confinement still apply.
- `max_concurrent` — how many runs of this agent one runner executes at
  once (default: unbounded).
- `token_epoch` — Unix seconds. Every token for this agent issued before it
  is refused (`agent_token_revoked`) from the next call on: the revocation of
  the external tokens `issue_agent_token` hands out, and of run tokens too.
  `escurel agent token revoke <id>` sets it to the next second.

## Choosing an agent for a run

1. An event's `provenance.agent: "[[agent::x]]"`, if `x` lists the skill.
2. Else the skill page's own `agent: "[[agent::x]]"` default.
3. Else no agent: the run keeps the runner's usual per-run token.

A skill whose default agent is missing, or does not list it, does not
run — the runner refuses it rather than falling back to a broader token.

## Which runner can run an agent

A **minting** runner — one holding the signing key
(`ESCUREL_RUNNER_AUTH_ISSUER` + `ESCUREL_RUNNER_AUTH_SIGNING_KEY`) — or a
runner on its own credential (`escurel:runner` or admin) whose gateway has a
signing key: it gets each run's token from the gateway (`mint_run_token`).
Where neither can mint a token as the agent, every run of a skill naming an
agent fails (dead-letters) rather than writing with the runner's own token.

## External agents

An admin issues a token for an agent that runs outside escurel (Claude Code,
Codex, Multica) with `escurel agent token issue <id>` (`issue_agent_token`):
`sub: agent:<id>`, the issuing admin as `act.sub`, the same scopes as a run.
`escurel agent token revoke <id>` sets `token_epoch`.

## Enforcement

The run's token names the agent (`agent` claim, `sub: agent:<id>`, role
`escurel:agent`, never `escurel:admin`). The gateway reads this page itself
on every call — the token never carries the scopes — and refuses every
tool with `agent_unknown` when the page is missing.
