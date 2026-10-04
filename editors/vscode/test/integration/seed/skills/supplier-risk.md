---
kind: skill
id: supplier-risk
folder: sales/risk
role: process
tags: [risk]
description: A supplier-risk signal, folded into the customer order it concerns.
autonomy: review
actions:
  - { name: review-order, kind: event, label: 'Review the order', event: customer-order }
---

# supplier-risk

Fold the incoming signal into the customer-order instance it names.

`autonomy: review`, so the runner holds its write as a draft and a human's promotion is what
publishes it. The write lands on a `customer-order`, a DIFFERENT skill from this one: that is
what makes the promotion a cascade hop (a same-skill write never cascades). `actions` is an
allow-list on top of that — empty means any skill, and one that does not name `customer-order`
BLOCKS the hop, which is how the integration test proves it is looking at a real cascade
(`runner-core/src/cascade.rs`).
