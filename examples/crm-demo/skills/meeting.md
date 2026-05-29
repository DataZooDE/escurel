---
type: skill
id: meeting
description: A meeting or call recording artifact with a captured transcript, from a calendar/conferencing source.
required_frontmatter: [at, source, channel]
optional_frontmatter: [participants, duration_min, subject, provenance, extracted_by, promoted_at, derived_from, about]
---

# meeting

A meeting artifact — a call or recording captured from a `source`
(e.g. `gcal` / `meet`) with its transcript. Like an `[[email::*]]`,
it is raw material: agents read the transcript and promote facts onto
typed instances, recording provenance here.

## Required fields

- `at` — ISO-8601 timestamp (UTC) the meeting started
- `source` — `gcal`, `meet`, `zoom`, `teams`
- `channel` — `recording` / `notes`

## Optional fields

- `participants` — list of `[[contact::*]]` / external names
- `duration_min`, `subject`
- `provenance` — `EXTRACTED` / `AUTO-PROMOTED`
- `extracted_by` — agent id
- `promoted_at` — ISO-8601 timestamp facts were auto-promoted
- `derived_from` — upstream artifact
- `about` — the typed instance this meeting most concerns
