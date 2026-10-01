---
type: skill
id: supplier-risk
description: A supplier-risk signal, folded into the customer order it concerns.
autonomy: review
actions: [customer-order]
---

# supplier-risk

Fold the incoming signal into the customer-order instance it names.

`autonomy: review`, so the runner holds its write as a draft and a human's promotion is what
publishes it. The change lands on a `customer-order`, a DIFFERENT skill from this one, and
`actions` lists it: both are conditions for the promotion to cascade (see
`runner-core/src/cascade.rs`).
