---
type: skill
id: stakeholder
description: A person or role whose goals, priorities and constraints shape the project. The root of the expectation graph.
required_frontmatter: [name, role]
optional_frontmatter: [org, influence, prev_stakeholder]
---

# stakeholder

A person or role who wants something from the project. Goals are
`held_by` a stakeholder; decisions are `decided_by` one. Stable
entities — a change of role is a new stakeholder linked via
`prev_stakeholder:`.

## Required fields

- `name` — display name (string)
- `role` — their role relative to the project (e.g. `VP Marketing`)

## Optional fields

- `org` — organisation / business unit
- `influence` — free-form note on decision weight (`sponsor`, `advisor`)
- `prev_stakeholder` — `[[stakeholder::*]]` this one succeeds
