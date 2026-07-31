---
type: skill
id: result
description: A measured finding produced by an analysis. Supports or refutes a hypothesis; chains via prev_result on a metric.
required_frontmatter: [at, statement, produced_by]
optional_frontmatter: [supports, refutes, prev_result, metric, value]
---

# result

A measured finding. Always `produced_by` an analysis; it `supports` or
`refutes` a hypothesis, and may chain to the prior measurement of the
same metric via `prev_result:`.

## Required fields

- `at` — when the result was measured (ISO-8601)
- `statement` — the finding in words
- `produced_by` — `[[analysis::*]]` that produced it

## Optional fields

- `supports` — `[[hypothesis::*]]` the evidence confirms
- `refutes` — `[[hypothesis::*]]` the evidence disconfirms
- `prev_result` — `[[result::*]]` the prior value of this metric
- `metric` — metric name (e.g. `auc`)
- `value` — the measured value
