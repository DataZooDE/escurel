---
type: skill
id: constraint
description: A limit the project must respect (budget, deadline, regulatory, data-access). Constrains goals and analyses; drifts like expectations do.
required_frontmatter: [at, statement, constrains, status]
optional_frontmatter: [supersedes, kind, severity]
---

# constraint

A limit the project must respect. Event-typed and revisable — a relaxed
or tightened constraint `supersedes` its predecessor.

## Required fields

- `at` — when the constraint was recorded (ISO-8601)
- `statement` — the limit (e.g. "no PII may leave the EU region")
- `constrains` — `[[goal::*]]` or `[[analysis::*]]` it limits
- `status` — `active` | `relaxed` | `superseded`

## Optional fields

- `supersedes` — `[[constraint::*]]` this one replaces
- `kind` — `budget` | `time` | `legal` | `data` | `technical`
- `severity` — free-form (`hard` / `soft`)
