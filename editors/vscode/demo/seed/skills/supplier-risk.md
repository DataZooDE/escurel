---
type: skill
id: supplier-risk
description: A supplier-risk signal from purchasing - a vendor's confirmation date moved, a quantity was cut, or its rating changed - folded into the sales orders it affects.
autonomy: review
actions:
  - name: update-order
    kind: event
    label: Update the order
    event: customer-order
  - name: write-analysis
    kind: event
    label: Write the analysis
    event: supplier-risk-analysis
---

# supplier-risk

Fold the incoming signal into the sales order it names (the order whose item depends on the
vendor's purchase order): update the delivery risk, move the confirmed delivery date if the
vendor's confirmation moved, and add a line to the order's history.

`autonomy: review`, so the runner holds its write as a draft and a human's promotion is what
publishes it. The write lands on a `customer-order`, a DIFFERENT skill from this one, which is
what makes the promotion a cascade hop. `actions` is an allow-list on top of that.

The run also persists what it worked out as a `supplier-risk-analysis` instance, in the same
changeset as its change to the order: the supplier, every order that uses the affected material, the
net value at risk per order, and the follow-ups. The order links to it.
