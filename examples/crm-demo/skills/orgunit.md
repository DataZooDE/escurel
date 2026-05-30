---
type: skill
id: orgunit
description: A buying centre or division inside a customer — the organisational unit an engagement actually lands in, below the legal account.
required_frontmatter: [name, customer]
optional_frontmatter: [head, function, engagement, headcount, notes]
---

# orgunit

An **org unit** is a division or buying centre inside a
[[customer::*]] — e.g. "Plant Engineering" or "Clinical Data
Platform". It sits below the legal account and above the people:
engagements, projects and workstreams attach to an org unit, not to
the bare customer, so the same account can run several independent
motions in parallel.

Entity-bound (no `at:`): an org unit is a durable record, not an
event. It links *up* to its `[[customer::*]]` and is referenced *down*
by the engagement spine, contacts (`orgunit:`) and workstreams.
