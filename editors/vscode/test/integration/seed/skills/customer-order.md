---
type: skill
id: customer-order
description: A customer order. A change to one is announced.
autonomy: review
cascade:
  target: produced
---

# customer-order

`cascade: { target: produced }` routes the follow-on event to the order that was just written,
which is the third condition for a hop. The thread then shows the cascade as a second event
under the run that caused it.
