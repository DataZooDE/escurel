---
kind: skill
id: chat-channel
description: Which escurel group a shared chat (a Teams group chat or channel)
  acts as when a chat bridge answers in it. Without a mapping the bridge
  declines to answer there.
required_frontmatter: [channel, group]
optional_frontmatter: [conversation_id, team_id, channel_id, archived]
instance_acl: false
acl:
  read: [admin]
  create: [admin]
  update: [admin]
  delete: [admin]
---

# chat-channel — who a shared chat acts as

A system skill, shipped into every tenant. Only `escurel:admin` may create,
change, delete or read a mapping; the gateway refuses everyone else whatever
the tenant's ACL defaults say.

A 1:1 chat with the bot acts as the person. A group chat or a channel is
shared: an answer there is read by everyone in it, so it must not carry one
member's private access. It acts as the GROUP principal `group:<group>`
instead, holding only `<group>`'s access, and only once an admin maps it here.
The person who asked is recorded alongside (`asked_by`, and `act.sub` on the
turn's token), and must be a member of `<group>` in the directory, so a
stranger added to a mapped chat cannot use the group's access.

## Fields

- `channel` — the bridge channel, `msteams`.
- `group` — the escurel group the conversation acts as. Never `public`,
  `owner`, `admin` or an `escurel:` role.
- Exactly one of:
  - `conversation_id` — a group chat's conversation id (`19:…@thread.v2`), or
  - `team_id` and `channel_id` — a channel in a team; every thread in it.
- `archived: true` — the mapping no longer applies.

The bridge's refusal in an unmapped chat names the ids to put here.
