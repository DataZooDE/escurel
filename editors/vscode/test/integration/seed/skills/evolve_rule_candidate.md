---
kind: skill
id: evolve_rule_candidate
description: An owner-private batch of discovered data rules awaiting a test or a human decision.
owner_field: owner_subject
acl:
  read: [owner]
  create: [owner]
  update: [admin]
required_frontmatter: [owner_subject, status]
---
# Evolve rule candidate
