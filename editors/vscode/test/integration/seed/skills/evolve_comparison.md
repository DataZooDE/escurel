---
kind: skill
id: evolve_comparison
description: An owner-private comparison of two programs of an Evolve experiment, materialised by Evolve.
owner_field: owner_subject
acl:
  read: [owner]
  create: [owner]
  update: [admin]
required_frontmatter: [owner_subject, experiment, status]
actions:
  - {name: compute-comparison, kind: event, label: Compute comparison, event: evolve_compare}
---
# Evolve comparison
