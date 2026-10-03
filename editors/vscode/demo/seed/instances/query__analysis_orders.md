---
type: instance
skill: query
id: analysis_orders
target: "[[order-lines::all]]"
params:
  - {name: analysis, type: string, required: true}
sql: "SELECT order_id AS \"order\", customer, qty, net_value, ROUND(100.0 * net_value / SUM(net_value) OVER (), 1) AS share_pct FROM {{target}} WHERE starts_with(:analysis, supplier || '-') ORDER BY net_value DESC"
---

# analysis_orders

The orders a supplier-risk analysis is about, one row per order line, largest first, with each line's
share of the analysis's total. It feeds the bar chart of the `supplier-risk-report`; the same numbers
are the table in the analysis page itself.

An analysis id starts with the slug of the supplier it analyses (`meier-guss-…`), which is how the query
knows which lines to take. The `analysis` parameter is the only one: Peacock binds the report's whole
parameter vector to every query, so the query declares it.
