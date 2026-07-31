---
type: skill
id: hypothesis
description: A falsifiable claim linking an expected outcome to data. Tests a goal/expectation; supported or refuted by results. Its status is the spine of project memory.
required_frontmatter: [at, statement, tests, status]
optional_frontmatter: [abandoned_because, supersedes, confidence]
---

# hypothesis

A falsifiable claim — the spine of the knowledge graph and the bridge to
the expectation graph (it `tests` an expectation). Results `support` or
`refute` it; an abandoned line is `status: abandoned` with an
`abandoned_because:` note.

## Required fields

- `at` — when the hypothesis was formed (ISO-8601)
- `statement` — the falsifiable claim
- `tests` — `[[expectation::*]]` or `[[goal::*]]` it probes
- `status` — `open` | `supported` | `refuted` | `abandoned`

## Optional fields

- `abandoned_because` — `[[decision::*]]` or `[[expectation::*]]` that ended it
- `supersedes` — `[[hypothesis::*]]` a reframing replaces
- `confidence` — subjective prior (`high`/`medium`/`low`)
