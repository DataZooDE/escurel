---
type: skill
id: playbook
description: Firm-authored engagement playbook (crm-essentials v1).
layer: base@crm-essentials@v1
required_frontmatter: [name]
optional_frontmatter: [stage_gates, review_cycle_days]
stage_gates: 4
review_cycle_days: 90
---

# playbook

The firm-authored canonical engagement playbook, shipped with the
`crm-essentials` pack. Base-layer pages like this one are read-only at
the subscribing node (the server rejects writes with
`layer_read_only`); a tenant specialises the playbook by authoring an
**overlay** skill of the same id — see
`examples/crm-demo/skills/playbook.md`, which shadows this page
(REQ-LAYER-03).

## Canonical stage gates

1. Discovery — a first `[[engagement::*]]` is logged.
2. Qualification — the `[[lead::*]]` clears BANT.
3. Proposal — an `[[opportunity::*]]` carries a value and a close date.
4. Commitment — the change order is signed.

Review the playbook every `review_cycle_days` days.
