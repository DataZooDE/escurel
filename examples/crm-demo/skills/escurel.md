---
type: skill
id: escurel
description: How this knowledge base is organised and how to navigate it. Read this first when entering a new tenant.
required_frontmatter: []
optional_frontmatter: []
---

# escurel — how this knowledge base is organised

This tenant is a small **CRM example**. Two kinds of pages live here:

- **Skills** (`type: skill`) are *type declarations*. Each one names a
  conceptual entity (customer, contact, engagement, lead, ...) and
  declares which frontmatter fields its instances must carry.
- **Instances** (`type: instance`) are *memories of a skill*. Each
  instance cites its skill via `skill: <skill-id>` in frontmatter,
  carries the required fields, and may link to other pages with
  `[[skill::id]]` wikilinks.

## Reading order

1. Start at `skills/` to see the entity model.
2. Open `instances/` and walk the Hoffmann chain
   (`engagement` → `lead` → `opportunity`) by following the
   `prev_event` and `follow_up` wikilinks.
3. Refer back to the [skill-based CRM brief][brief] for the
   conceptual framing.

## Wikilink syntax

```text
[[skill::id]]
[[skill::id|alias]]
[[skill::id#anchor]]
[[skill::id@version]]
```

All segments after `id` are optional. A bare `[[id]]` is resolvable
when there is no ambiguity.

[brief]: ../../../docs/notes/
