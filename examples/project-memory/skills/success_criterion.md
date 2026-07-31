---
type: skill
id: success_criterion
description: A measurable bar that decides whether a goal is met (e.g. "churn AUC >= 0.80 on holdout").
required_frontmatter: [name, threshold]
optional_frontmatter: [metric, measured_on]
---

# success_criterion

A measurable bar. Goals cite one via `measured_by:`; a `result` meeting
the bar is the evidence a goal is met.

## Required fields

- `name` — short label (e.g. `churn-auc-80`)
- `threshold` — the bar (e.g. `AUC >= 0.80`)

## Optional fields

- `metric` — the metric name (e.g. `auc`)
- `measured_on` — the dataset/holdout the bar applies to
