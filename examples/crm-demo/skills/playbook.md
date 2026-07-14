---
type: skill
id: playbook
description: Demo-specialised engagement playbook (shadows crm-essentials v1).
required_frontmatter: [name]
optional_frontmatter: [stage_gates, review_cycle_days]
stage_gates: 5
review_cycle_days: 90
---

# playbook

The demo tenant's **overlay** specialisation of the firm-authored
`playbook` skill from the `crm-essentials` pack. It shadows the base
page (`base/crm-essentials/skills/playbook.md`) without forking it:
`resolve` and `list_skills` prefer this overlay, while `expand` keeps
the shadowed base's frontmatter visible under `shadow.base` so drift —
here `stage_gates: 5` vs the canonical `4`, and the reworded
description — is never silently masked (REQ-LAYER-03).

## Demo stage gates

1. Discovery — a first `[[engagement::*]]` is logged.
2. Qualification — the `[[lead::*]]` clears BANT.
3. Technical validation — the pilot `[[project::*]]` runs. *(the demo
   tenant's extra gate)*
4. Proposal — an `[[opportunity::*]]` carries a value and a close date.
5. Commitment — the change order is signed.
