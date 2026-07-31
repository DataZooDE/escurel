---
type: skill
id: decision
description: A committed choice. Motivated by expectations (why), justified by results (evidence), made by a stakeholder. The bridge between the expectation and knowledge graphs and the primary "why" record.
required_frontmatter: [at, title, decided_by, status]
optional_frontmatter: [motivated_by, justified_by, addresses, abandons, abandoned_because, supersedes]
---

# decision

A committed choice — the primary answer to "why did we do this / why did
we stop." The **bridge** entity: `motivated_by` points into the
expectation graph (why we wanted it), `justified_by` into the knowledge
graph (what the data said). A decision that rested on an expectation
that was later superseded is `status: orphaned` — the drift the memory
is built to surface.

## Required fields

- `at` — when the decision was made (ISO-8601)
- `title` — the choice
- `decided_by` — `[[stakeholder::*]]` who made it
- `status` — `active` | `reversed` | `orphaned`

## Optional fields

- `motivated_by` — `[[goal::*]]`/`[[expectation::*]]`/`[[constraint::*]]` — the why
- `justified_by` — `[[result::*]]`/`[[hypothesis::*]]` — the evidence
- `addresses` — `[[hypothesis::*]]`/`[[analysis::*]]` the decision governs
- `abandons` — `[[hypothesis::*]]`/`[[analysis::*]]` being dropped
- `abandoned_because` — `[[expectation::*]]`/`[[decision::*]]` that ended a path
- `supersedes` — `[[decision::*]]` this reverses/replaces
