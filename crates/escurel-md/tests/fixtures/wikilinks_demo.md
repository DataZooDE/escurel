---
type: instance
skill: weekly-review
id: 2026-w20
at: 2026-05-18T17:00:00+02:00
---

# Weekly review — 2026-w20

Spent most of the week on [[customer::acme-corp]] follow-ups. The
QBR notes are in [[meeting::2026-04-12-acme-qbr#blk-acme-signals]];
the renewal terms pin against [[contract::acme-2025-renewal@v3]].

For background context, see the [[error-catalogue]] and the
[[customer::acme-corp|Acme Corp]] one-pager. The escalation chain
ends at [[person::erika-mustermann#blk-on-call@v2|Erika (on-call)]].

## Things to ignore

Inline code such as `[[not-a-link]]` and `[[another::ignored]]`
must NOT be parsed as wikilinks.

A fenced block:

```text
[[fenced::skipped]] [[also::skipped]]
```

And a Rust block with a wikilink inside a comment:

```rust
// [[rust::comment-also-skipped]]
let _ = 1;
```

After the fenced regions close, parsing resumes:
[[note::wrap-up-2026-w20]].
