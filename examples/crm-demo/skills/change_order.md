---
type: skill
id: change_order
description: A mid-delivery scope change against a project. Re-prices and re-baselines without spawning a new opportunity.
required_frontmatter: [at, status, project]
optional_frontmatter: [delta_value_eur, split, scope, accepted_at, prev_event]
---

# change_order

A change order records a scope change negotiated *during* delivery.
It attaches to a `[[project::*]]` (and, through it, to the engagement
spine) rather than reopening the sales funnel — the accumulated
context stays in one place.

## Required fields

- `at` — ISO-8601 timestamp (UTC) the change was raised
- `status` — `raised` / `negotiating` / `accepted` / `rejected`
- `project` — `[[project::*]]` the change applies to

## Optional fields

- `delta_value_eur` — signed value delta the change introduces
- `split` — commercial split, e.g. `60/40`
- `scope` — short description of what moved
- `accepted_at` — ISO-8601 timestamp once accepted
- `prev_event` — the artifact or event that surfaced the change
