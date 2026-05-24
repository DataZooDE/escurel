---
type: skill
id: customer
description: A buying organisation. Aggregates contacts, engagements, opportunities and projects under one entity.
required_frontmatter: [name, country]
optional_frontmatter: [industry, employee_band, primary_owner]
---

# customer

The buying organisation. Every customer-facing instance — contact,
engagement, lead, opportunity, project — eventually ties back to a
single `[[customer::*]]` page via its `customer:` frontmatter or via
an intermediate `[[contact::*]]`.

## Required fields

- `name` — display name (string)
- `country` — ISO-3166-1 alpha-2 (e.g. `DE`, `CH`)

## Optional fields

- `industry` — free-form vertical tag
- `employee_band` — `<10` / `10-99` / `100-999` / `1000-9999` / `10000+`
- `primary_owner` — `[[contact::*]]` of the AE (account executive)

## Notes

Customers are stable; renames go via versioned `update_page` rather
than file rename. Mergers/acquisitions are modelled as a new customer
that links back to its predecessor with `superseded_by:`.
