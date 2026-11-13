---
kind: skill
id: runtime
description: A runner registration — which harnesses (and CLI versions) a runner
  offers, the labels it carries and how many runs it takes at once. Written by
  the runner itself; liveness comes from its status record, not this page.
required_frontmatter: []
optional_frontmatter: [runner_id, harnesses, labels, max_concurrent, claim_holder]
instance_acl: false
acl:
  read: [public]
  create: [admin]
  update: [admin]
  delete: [admin]
---

# runtime — where work can run

A system skill, shipped into every tenant. Each `escurel-runner` writes one
instance, `markdown/instances/runtime/<runner-id-slug>.md`, on boot and again
whenever its probe of the harness CLIs changes. Only `escurel:admin` may
create, change or delete one, so the runner writes it with its own admin
identity (never a run token). A runner that cannot write its page logs it once
and keeps working: the page is a directory entry, not a precondition.

## Fields

- `runner_id` — `ESCUREL_RUNNER_ID`, as written (the slug is derived from it).
- `harnesses` — `[{name, version}]`: the runner's default harness and every
  one in `ESCUREL_RUNNER_HARNESS_ALLOW`, with the first line of
  `<cli> --version` (`null` for an in-process harness or a failed probe).
  Re-probed every `ESCUREL_RUNNER_PROBE_INTERVAL`.
- `labels` — `ESCUREL_RUNNER_LABELS`, e.g. `["site:muc", "has:sap-gui"]`.
- `max_concurrent` — how many harness processes the runner runs at once.
- `claim_holder` — the holder its `claim_event` leases carry (`<sub>#<ledger
  identity>`), so a reader can tell which runtime holds an inbox event.

## Liveness

The page is not rewritten per heartbeat. `list_runtimes` joins each page with
the runner's `runner-status:<runner_id>` record and derives `last_seen` and a
status: `online`, `stale` (silent for more than 3 x its reporting interval),
`stopped` (it said so on shutdown) or `unknown` (no record).

## Routing

A skill page or an event's `provenance` may declare
`requires: {harness: <name>, labels: [<label>, …]}`. A runner takes the work
only when it offers that harness and carries every label; it skips anything
else *before* claiming it, so unrouteable work stays visible in the inbox.
