---
kind: skill
id: supplier-risk-report
render: a2ui
description: One supplier-risk analysis as a report - the facts, the net value at risk per order as a bar chart, and the analysis text.
params:
  analysis: { type: string }
data:
  orders: "[[query::analysis_orders]]"
instances:
  a: "[[supplier-risk-analysis::{analysis}]]"
views:
  - { kind: frontmatter, instance: a, keys: [supplier, risk_level, risk_score, orders_affected, net_value_at_risk], label: Analysis }
  - { kind: vega, data: orders, spec: value_per_order }
  - { kind: table, data: orders }
  - { kind: markdown, instance: a }
specs:
  value_per_order:
    title: Net value at risk per order
    description: "Bar chart of the net value each affected order carries. The same figures, with the takeaway in words, are in the analysis text and its table."
    mark: bar
    encoding:
      x: { field: order, type: nominal, title: Order }
      y: { field: net_value, type: quantitative, title: Net value at risk }
---

Peacock report behind the `viewer:` of `supplier-risk-analysis`. The bar chart reads the rows of the
authored query `analysis_orders` (one row per affected order); the analysis body repeats the same
numbers as a table and states the takeaway in a sentence, so nothing is lost without the chart.
