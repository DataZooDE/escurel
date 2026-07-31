---
type: skill
id: priority
description: A named priority level goals are ranked against (e.g. must-have, nice-to-have).
required_frontmatter: [name, rank]
optional_frontmatter: [description]
---

# priority

A named ranking level. Goals cite a priority via `prioritized_by:`.
A small, stable vocabulary per project.

## Required fields

- `name` — display name (e.g. `must-have`)
- `rank` — integer sort key (lower = higher priority)

## Optional fields

- `description` — what the level means
