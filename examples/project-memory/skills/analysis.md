---
type: skill
id: analysis
description: A unit of analytical work over one or more datasets. Produces results; its provenance is derived_from / uses.
required_frontmatter: [at, title, uses, status]
optional_frontmatter: [derived_from, method, abandoned_because, supersedes]
---

# analysis

A unit of analytical work. Consumes datasets (`uses:`), may build on
prior work (`derived_from:`), and produces results (a `result` points
back with `produced_by:`).

## Required fields

- `at` — when the analysis was run (ISO-8601)
- `title` — what was done
- `uses` — `[[dataset::*]]` it consumes (one or many)
- `status` — `planned` | `running` | `done` | `abandoned`

## Optional fields

- `derived_from` — `[[analysis::*]]` or `[[result::*]]` it builds on
- `method` — technique (e.g. `gradient boosting`)
- `abandoned_because` — `[[decision::*]]` that ended it
- `supersedes` — `[[analysis::*]]` a rerun replaces
