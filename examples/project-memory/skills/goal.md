---
type: skill
id: goal
description: A desired outcome a stakeholder wants from the project. Refined by expectations, ranked by priority, judged by success-criteria.
required_frontmatter: [at, title, held_by, status]
optional_frontmatter: [prioritized_by, measured_by, supersedes, description]
---

# goal

A desired outcome. Event-typed (`at:`) because goals are stated at a
time and evolve; a revised goal `supersedes` its predecessor.

## Required fields

- `at` — when the goal was stated (ISO-8601)
- `title` — short statement of the outcome
- `held_by` — `[[stakeholder::*]]` who wants it
- `status` — `active` | `met` | `dropped` | `superseded`

## Optional fields

- `prioritized_by` — `[[priority::*]]` ranking this goal
- `measured_by` — `[[success_criterion::*]]` that decides "met"
- `supersedes` — `[[goal::*]]` this goal replaces
- `description` — longer prose
