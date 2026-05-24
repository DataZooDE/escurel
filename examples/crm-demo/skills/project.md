---
type: skill
id: project
description: A delivery engagement spawned at commercial close. Inherits opportunity context as live obligations.
required_frontmatter: [opened, status, customer, opportunity]
optional_frontmatter: [phase, lead_pm, scope_clarity, margin_health, schedule_confidence, acceptance_state]
---

# project

A project starts the moment an `[[opportunity::*]]` reaches `won`.
Sales commitments inherited from the opportunity become live delivery
obligations — they do not get copied; the project reads them via the
shared customer + opportunity pages.

## Required fields

- `opened` — date the project was kicked off
- `status` — `pending` / `delivering` / `accepted` / `cancelled`
- `customer` — `[[customer::*]]`
- `opportunity` — `[[opportunity::*]]` that produced this project

## Optional delivery dimensions

- `phase` — current milestone or workstream name
- `lead_pm` — `[[contact::*]]` of the delivery lead (typically ours)
- `scope_clarity` — 0.0-1.0 (inherited from opportunity at close)
- `margin_health` — 0.0-1.0 (rolling)
- `schedule_confidence` — 0.0-1.0 (rolling)
- `acceptance_state` — `not-started` / `partial` / `accepted` / `disputed`

## Notes

Change orders are spawned as separate `[[project::*]]` instances with
`prev_review:` linking back, not as in-place mutations of the
original. Renewals spawn `[[opportunity::*]]` instances with
MEDDPICC dimensions pre-filled from the project's delivery trace.
