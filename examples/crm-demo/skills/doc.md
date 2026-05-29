---
type: skill
id: doc
description: A document artifact (drive/sharepoint) captured into the knowledge base, often an agent-generated analysis.
required_frontmatter: [at, source, channel]
optional_frontmatter: [title, author, provenance, extracted_by, promoted_at, derived_from, about]
---

# doc

A document artifact — a file captured from a `source` (e.g. `drive`)
or produced by an agent (a status report, a detector scan). Docs are
where auto-promotion is most visible: an agent writes a doc, extracts
a fact, and promotes it onto a typed instance, all recorded via the
provenance fields.

## Required fields

- `at` — ISO-8601 timestamp (UTC)
- `source` — `drive`, `sharepoint`, `agent`
- `channel` — `docs` / `report`

## Optional fields

- `title`, `author`
- `provenance` — `EXTRACTED` / `AUTO-PROMOTED`
- `extracted_by` — agent id
- `promoted_at` — ISO-8601 timestamp facts were auto-promoted
- `derived_from` — upstream artifact
- `about` — the typed instance this doc most concerns
