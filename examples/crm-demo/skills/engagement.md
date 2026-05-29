---
type: skill
id: engagement
description: A first-touch interaction with a contact, or the continuous lifecycle spine for an account.
required_frontmatter: [at, with, channel]
optional_frontmatter: [outcome, follow_up, notes, spine, template, phase, commercial_model, contract_value, running_margin, risk_surface, sentiment_trend, primary_sponsor, champion, orgunit, customer]
---

# engagement

An engagement is a single first-contact event with a
`[[contact::*]]`. It is what gets recorded when someone hits the
booth, downloads a whitepaper, replies to a cold email, or shows up
on a discovery call.

## Required fields

- `at` — ISO-8601 timestamp (UTC)
- `with` — `[[contact::*]]` we engaged
- `channel` — one of `event`, `email`, `call`, `web`, `referral`

## Optional fields

- `outcome` — `cold` / `warm` / `hot` / `no-fit`
- `follow_up` — `[[lead::*]]` spawned by this engagement (when one is)
- `notes` — free-form markdown summary

## Notes

Engagements are append-only — never edited after the day they
happened. If a follow-up reveals new context about the conversation,
add a new engagement that cites it via `prev_event:`.

## The engagement spine (`spine: true`)

A second use of this skill is the **lifecycle spine**: one engagement
instance that is the continuous parent of an account's
`[[lead::*]]` → `[[opportunity::*]]` → `[[project::*]]` →
`[[change_order::*]]` → `[[renewal::*]]` chain — *one skill instance for
the whole lifecycle*, not a copy-forward chain. Promises made during
the sale attach to the spine and become delivery obligations by
construction. A spine instance sets `spine: true` and carries the
lifecycle fields:

- `template` — the spine template, e.g. `engagement_skill v1`
- `phase` — `qualifying` / `won` / `delivering` / `renewing`
- `commercial_model`, `contract_value`, `running_margin`, `risk_surface`,
  `sentiment_trend` — the live commercial signals
- `primary_sponsor`, `champion` — `[[contact::*]]`
- `orgunit`, `customer` — `[[customer::*]]` the spine belongs to
