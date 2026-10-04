---
kind: skill
id: evolve_validation_report
description: Owner-scoped evidence from the synthetic holdout replay.
owner_field: owner_subject
acl:
  read: [owner]
  create: [admin]
  update: [admin]
required_frontmatter: [owner_subject]
actions:
  - {name: create-policy-candidate, kind: event, label: Create policy candidate, event: evolve_publish_candidate}
---
# Evolve validation report
