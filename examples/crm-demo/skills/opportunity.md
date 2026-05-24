---
type: skill
id: opportunity
description: A named, valued sales motion. The MEDDPICC stage of the funnel.
required_frontmatter: [opened, status, customer, value_eur]
optional_frontmatter: [contact, close_date, prev_review, competition, economic_buyer, decision_process, decision_criteria, identified_pain, metrics, champion]
---

# opportunity

An opportunity is a sales motion with a name, a number, and a close
date. Qualification has moved past BANT — we're now tracking the
deeper MEDDPICC dimensions through to commercial close.

## Required fields

- `opened` — date opened (typically the day the lead converted)
- `status` — `open` / `negotiating` / `closing` / `won` / `lost`
- `customer` — `[[customer::*]]` buying
- `value_eur` — annual contract value, round number

## Optional MEDDPICC dimensions

- `metrics` — quantified business outcome
- `economic_buyer` — `[[contact::*]]` who signs
- `decision_criteria` — buyer's stated rubric
- `decision_process` — known stages and dates of their buying cycle
- `identified_pain` — what hurts today
- `champion` — `[[contact::*]]` defending the deal internally
- `competition` — who else is in the room
- `close_date` — target signature date
- `contact` — primary point of contact
- `prev_review` — `[[opportunity::*]]` if this superseded an earlier shot

## Lifecycle

- `open` → `negotiating` once a champion is verified
- `negotiating` → `closing` once commercial terms are on the table
- `closing` → `won` triggers project spawn
- any → `lost` ends the motion; the page persists as memory
