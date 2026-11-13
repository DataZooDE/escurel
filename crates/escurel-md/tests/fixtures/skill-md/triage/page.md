---
kind: skill
id: triage
description: 'Triage an inbound support request: sort it by urgency and owner.'
summary: Sort a request by urgency and owner.
autonomy: review
schedule: 0 7 * * 1-5
tags:
- support
- inbox
acl:
  read:
  - public
  create:
  - editors
skill_md:
  license: MIT
  metadata:
    author: support-team
---
# triage

Look the requester up first: [[customer::acme|Acme]] is the reference account.
Compare with the [[customer]] skill and its [[customer#fields]] before you answer,
then file the outcome under [[ticket::open@v2]]. Again: [[customer::acme]].

Code is not a link: `[[not::a-link]]`.

```
[[also::not-a-link]]
```
