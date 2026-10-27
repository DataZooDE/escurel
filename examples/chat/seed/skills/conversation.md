---
kind: skill
id: conversation
description: A private conversation with the knowledge agent — one owner, one chat.
owner_field: owner
acl:
  read: [owner]
  create: [owner]
  update: [owner]
  delete: [owner]
chat:
  page_bound: true
required_frontmatter: [title, owner, started_at]
optional_frontmatter: [about, focus, summary]
---
# conversation

A conversation with the knowledge agent. `owner` is the person's token subject; `about` links the record it is about, `focus` the pages pinned as context.
