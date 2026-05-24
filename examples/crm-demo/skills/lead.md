---
type: skill
id: lead
description: A qualified follow-up under BANT. Lives between engagement and opportunity.
required_frontmatter: [opened, status, contact]
optional_frontmatter: [budget, authority, need, timing, expected_value_eur, prev_event]
---

# lead

A lead exists once a `[[contact::*]]` has indicated genuine interest
worth investing AE time in. We qualify under BANT — *budget,
authority, need, timing* — and each dimension fills in as discovery
progresses.

## Required fields

- `opened` — date the lead was created
- `status` — `open` / `qualified` / `disqualified` / `converted`
- `contact` — `[[contact::*]]` champion

## Optional fields (BANT dimensions)

- `budget` — confirmed / hypothesised / unknown
- `authority` — `[[contact::*]]` of the decision maker (may equal `contact`)
- `need` — free-form one-liner
- `timing` — quarter or date when the buyer wants the outcome
- `expected_value_eur` — round number, the lead's ballpark ARR
- `prev_event` — `[[engagement::*]]` the lead spawned from

## Lifecycle

- `open` → `qualified` when BANT vector norm crosses ~0.8
- `qualified` → `converted` when an `[[opportunity::*]]` is spawned
- `qualified` → `disqualified` if the buyer falls out

Disqualified leads are kept (not deleted) so future signals can
re-open them.
