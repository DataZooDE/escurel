---
type: skill
id: project
description: A unit of work with a goal and a lifecycle. A sub-project is a project whose `part_of` names its parent; every work item scopes to its project. Closed with a conclusion.
required_frontmatter: [title, status]
optional_frontmatter: [at, part_of, held_by, concluded_by, description]
---

# project

A bounded effort with its own lifecycle. Projects **nest**: a
sub-project is just a `project` instance whose `part_of` points at its
parent, so a parent rolls up its children through the graph. Work items
(`goal`, `hypothesis`, `analysis`, `decision`, …) declare which project
they belong to with a `scope:` link.

Closing is a **status transition, not a delete**: flip `status` to
`closed` and point `concluded_by` at the [`conclusion`](conclusion.md)
that captures the takeaway. The record — and everything under it — stays
queryable.

## Required fields

- `title` — what the project is about
- `status` — `active` | `paused` | `closed`

## Optional fields

- `at` — when it opened (ISO-8601)
- `part_of` — `[[project::*]]` parent (present ⇒ this is a sub-project)
- `held_by` — `[[stakeholder::*]]` who owns it
- `concluded_by` — `[[conclusion::*]]` that closed it
- `description` — longer prose

## Relations work items use

- `scope: [[project::<id>]]` on a goal/hypothesis/analysis/decision/…
  places it inside a project. `neighbours(<project>, in, scope)`
  enumerates a project's contents; filtering the provenance walks by
  `scope` keeps a query inside one project.
