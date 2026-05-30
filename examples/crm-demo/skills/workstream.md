---
type: skill
id: workstream
description: A parallel stream of delivery work inside a project or engagement — the unit a delivery team actually plans and tracks against.
required_frontmatter: [name, status, project]
optional_frontmatter: [owner, engagement, customer, health, notes]
---

# workstream

A **workstream** is one parallel track of delivery inside a
[[project::*]] (and, by inheritance, its [[engagement::*]] spine) —
e.g. "first-unit provisioning", "3-site data model", "onboarding".
Multiple workstreams run concurrently under one project; each has its
own owner and health.

Entity-bound (no `at:`): a workstream is a durable record of a track
of work. It links *up* to its `[[project::*]]` / `[[engagement::*]]`
and is referenced as a delivery obligation of the spine.
