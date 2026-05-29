---
type: skill
id: renewal
description: A renewal cycle on a delivered engagement. Qualifies with priors instead of starting cold.
required_frontmatter: [at, status, project]
optional_frontmatter: [propensity, expected_value_eur, prev_event, analog]
---

# renewal

A renewal opens once a `[[project::*]]` is delivering or delivered.
Unlike a fresh lead it qualifies *with priors* — the inherited
commitments, sentiment, and installed base carry straight over from
the engagement spine.

## Required fields

- `at` — ISO-8601 timestamp (UTC) the renewal cycle opened
- `status` — `qualifying-with-priors` / `proposed` / `won` / `lost`
- `project` — the delivered `[[project::*]]` being renewed

## Optional fields

- `propensity` — model score `0.0`–`1.0`
- `expected_value_eur` — expected renewal value
- `prev_event` — the signal that opened the renewal
- `analog` — closest prior deal used as the renewal model
