---
type: skill
id: dataset
description: A data source an analysis consumes. The leaf of the knowledge graph's provenance chain.
required_frontmatter: [name, grain]
optional_frontmatter: [source, rows, prev_dataset]
---

# dataset

A data source. Analyses cite datasets via `uses:`; the dataset is the
leaf of the provenance chain `result → analysis → dataset`.

## Required fields

- `name` — display name (e.g. `customer-events`)
- `grain` — one row per … (e.g. `customer-day`)

## Optional fields

- `source` — where it comes from (system / table / query)
- `rows` — approximate row count
- `prev_dataset` — `[[dataset::*]]` this refreshes/replaces
