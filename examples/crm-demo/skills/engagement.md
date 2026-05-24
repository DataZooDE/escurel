---
type: skill
id: engagement
description: A first-touch interaction with a contact. The top of the funnel.
required_frontmatter: [at, with, channel]
optional_frontmatter: [outcome, follow_up, notes]
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
