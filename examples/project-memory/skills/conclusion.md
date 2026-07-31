---
type: skill
id: conclusion
description: The durable takeaway that closes a project — what was learned, the evidence, what was decided, and what is reusable. A first-class node later work links via `builds_on`; a parent conclusion `synthesizes` its sub-conclusions.
required_frontmatter: [at, concludes, statement]
optional_frontmatter: [supported_by, decided, synthesizes, supersedes, reusable]
---

# conclusion

The finding a project ends on. It is authored when a project closes and
persists as a **reusable, linkable node**: downstream work cites it with
`builds_on: [[conclusion::*]]`, so a provenance walk from new work reaches
the closed project's takeaway automatically. A parent project's
conclusion `synthesizes` its sub-projects' conclusions (rollup).

When a later conclusion `supersedes` an earlier one, the earlier is
*retired* — it surfaces in `abandoned_paths` (the "still building on an
overturned finding?" check), exactly like a superseded expectation.

## Required fields

- `at` — when the project was concluded (ISO-8601)
- `concludes` — `[[project::*]]` this closes
- `statement` — the takeaway, in words

## Optional fields

- `supported_by` — `[[result::*]]` evidence behind it
- `decided` — `[[decision::*]]` it committed to
- `synthesizes` — `[[conclusion::*]]` sub-conclusions rolled up (parent)
- `supersedes` — `[[conclusion::*]]` an earlier conclusion it overturns
- `reusable` — `true` when the takeaway generalises beyond this project
  (a candidate to promote into a firm-wide pack)
