---
type: skill
id: supplier-risk
description: A supplier-risk signal, folded into the order it concerns.
autonomy: review
cascade: [customer-notice]
---

# supplier-risk

Fold the incoming signal into the order instance it names.

`autonomy: review`, so the runner holds its write as a draft and a human's
promotion is what publishes it. Promoting cascades a `customer-notice` event,
which is the second hop M3's Thread view has to show appearing by itself.
