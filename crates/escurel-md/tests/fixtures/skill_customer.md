---
type: skill
id: customer
description: A buying entity (organisation or individual) that may have
  one or more contacts, deals and account-owning interactions.
required_frontmatter:
  - tier
  - opened
  - status
optional_frontmatter:
  - mrr_band
  - owner
  - segment
---

# customer

A customer is the unit of revenue. Every contract, decision-record and
weekly-review eventually traces back to one customer instance.

## Required fields

- `tier`: `enterprise` | `mid-market` | `smb`
- `opened`: ISO 8601 date the relationship started
- `status`: `prospect` | `active` | `churned`
