---
kind: skill
id: evolve_experiment
description: Owner-scoped synthetic Evolve experiment projection for native tests.
owner_field: owner_subject
acl:
  read: [owner]
  create: [admin]
  update: [admin]
required_frontmatter: [owner_subject, status]
actions:
  - {name: validate-winner, kind: event, label: Validate winner, event: evolve_validate}
---
# Evolve experiment
