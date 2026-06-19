---
type: skill
id: engagement
description: A first-touch interaction with a contact, or the continuous lifecycle spine for an account.
required_frontmatter: [at, with, channel]
optional_frontmatter: [outcome, follow_up, notes, spine, template, phase, commercial_model, contract_value, running_margin, risk_surface, sentiment_trend, primary_sponsor, champion, orgunit, customer]
acl:
  read:   [public]
  create: [admin]
  update: [admin, agent-writer]
  delete: [admin]
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

## Access control (group ACL v1)

This skill declares a group ACL (`acl:` in the header): instances are
**world-readable** (`read: [public]`), created/deleted by **admins**, and
**updated by admins OR the `agent-writer` group**. The agent-runner folds
inbound events into the spine via `update_page` — an *update* — so under an
auth-enforcing gateway the agent's token must carry `agent-writer` in its
`groups_claim` (or be admin). In open dev mode the gateway has no verifier,
so the caller is treated as admin and the fold is allowed unchanged. See
the README's **Auth** note and [`docs/adr/0004-rbac-groups.md`].

## Processing an inbound event (agent-runner contract)

When the agent runner hands you an inbound event that is pre-flagged to
a spine instance, fold it **into that same instance** — do not create a
new one:

1. `expand` the pre-flagged instance to read its current body + frontmatter.
2. Merge the event in: update the `## Status` section if the event changes
   delivery status, and append a short **dated** note under `## Notes`
   (`### <YYYY-MM-DD> — <one-line summary> (<event_id>)`). Preserve all
   existing content and the YAML frontmatter verbatim.
3. `update_page` that **same** instance with the merged markdown.
4. `assign_event` the event to that **same** instance to mark it
   `processed` and bound.

The runner confirms success by reading back that the event is `processed`
on the pre-flagged instance, so steps 3 and 4 must target it exactly.
