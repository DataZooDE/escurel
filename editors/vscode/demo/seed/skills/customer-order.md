---
type: skill
id: customer-order
description: A customer order. A change to one is announced.
autonomy: review
---

# customer-order

An order for a customer. When a signal about a supplier changes one, the change is announced as
a follow-on event, which the thread shows under the run that caused it.

The demo deliberately does not route that follow-on back onto the order
(`cascade: {target: produced}`): the echo harness always folds the OLDEST inbox event that has a
target page, so a dead-lettered follow-on left in the inbox would swallow the next run.
