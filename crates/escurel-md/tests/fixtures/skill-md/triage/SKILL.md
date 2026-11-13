---
name: triage
description: 'Triage an inbound support request: sort it by urgency and owner.'
license: MIT
metadata:
  author: support-team
  escurel:
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
---
# triage

Look the requester up first: [[customer::acme|Acme]] is the reference account.
Compare with the [[customer]] skill and its [[customer#fields]] before you answer,
then file the outcome under [[ticket::open@v2]]. Again: [[customer::acme]].

Code is not a link: `[[not::a-link]]`.

```
[[also::not-a-link]]
```

<!-- escurel:resolve-hints -->
## escurel references

The `[[…]]` links above name pages in an escurel knowledge base. Look one up with the escurel MCP `resolve` tool (`{"wikilink": "[[skill::id]]"}`), then `expand` the page it returns:

- `[[customer::acme]]`
- `[[customer]]`
- `[[customer#fields]]`
- `[[ticket::open@v2]]`
<!-- /escurel:resolve-hints -->
