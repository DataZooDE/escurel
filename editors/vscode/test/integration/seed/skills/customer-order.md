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
so the next hop's run reaches for that instance. It does not decide WHETHER there is a hop —
a cross-skill write does, filtered by the parent's `actions` — so removing it leaves the thread
unchanged. The thread shows the hop as a second event under the run that caused it.
