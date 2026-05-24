---
type: skill
id: contact
description: An individual person at a customer. The unit of human relationship.
required_frontmatter: [name, customer]
optional_frontmatter: [role, email, phone, linkedin]
---

# contact

An individual person. Every contact belongs to exactly one
`[[customer::*]]` via the `customer:` field; a person who moves jobs
gets a new contact page (with `succeeded_by:` linking the old to the
new).

## Required fields

- `name` — full display name
- `customer` — `[[customer::*]]` employer

## Optional fields

- `role` — job title, free-form
- `email`, `phone`, `linkedin` — channels

## Notes

A contact's relationship state with us lives on the
`[[engagement::*]]` and `[[lead::*]]` pages they appear on — *not* on
the contact itself. The contact page is the durable identity; the
funnel state is per-interaction.
