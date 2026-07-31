---
type: skill
id: expectation
description: A concrete, revisable statement of what a stakeholder expects to be true or delivered. Its supersession chain is the primary record of how expectations evolved.
required_frontmatter: [at, statement, refines, status]
optional_frontmatter: [supersedes, confidence, held_by]
---

# expectation

A concrete, revisable statement that makes a goal actionable — and **the
thing that drifts**. When an expectation changes, author a new instance
that `supersedes` the old one; the chain, ordered by `at`, is the record
of how the project's expectations evolved.

## Required fields

- `at` — when the expectation was stated (ISO-8601)
- `statement` — what is expected to be true / delivered
- `refines` — `[[goal::*]]` this expectation makes concrete
- `status` — `current` | `superseded` | `withdrawn`

## Optional fields

- `supersedes` — `[[expectation::*]]` this revision replaces
- `confidence` — subjective confidence (`high`/`medium`/`low`)
- `held_by` — `[[stakeholder::*]]` if narrower than the goal's holder
