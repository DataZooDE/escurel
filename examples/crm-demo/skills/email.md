---
type: skill
id: email
description: An inbound or outbound email artifact captured into the knowledge base from a source channel.
required_frontmatter: [at, source, channel]
optional_frontmatter: [from, to, subject, direction, provenance, extracted_by, promoted_at, derived_from, about]
---

# email

An email artifact — one message captured from a `source` (e.g.
`gmail`) into the tenant. Artifacts are the raw material the agents
read; facts they extract get promoted onto typed instances (a
`[[lead::*]]`, `[[opportunity::*]]`, …) and the artifact records the
provenance of that promotion.

## Required fields

- `at` — ISO-8601 timestamp (UTC)
- `source` — capture source: `gmail`, `outlook`, `imap`
- `channel` — `inbox` / `sent`

## Optional fields

- `from`, `to`, `subject`, `direction` (`inbound`/`outbound`)
- `provenance` — `EXTRACTED` / `AUTO-PROMOTED` (how facts left this artifact)
- `extracted_by` — agent id, e.g. `agt:qualifier-a`
- `promoted_at` — ISO-8601 timestamp facts were auto-promoted
- `derived_from` — upstream artifact this one threads from
- `about` — the typed instance this artifact most concerns
