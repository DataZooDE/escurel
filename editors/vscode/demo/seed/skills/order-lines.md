---
kind: skill
id: order-lines
description: The demo's sales-order lines as rows (a read-only SQL view over a JSON extract), so reports and charts can aggregate across orders.
backend:
  kind: sql_view
  source:
    connector: json_dir
    # DuckDB resolves a relative glob against the server's cwd, so demo/run.sh replaces this
    # placeholder with the absolute path of demo/sources/order-lines before the gateway starts.
    relation: "@ORDER_LINES_DIR@"
  search_text: [customer, material, supplier]
optional_frontmatter: [customer, supplier, material]
---

# order-lines

One row per item of a sales order: `order_id`, `item`, `customer`, `supplier` (the slug of the vendor
that supplies the material), `material`, `description`, `qty`, `net_value`, `currency`.

It exists so a chart can read numbers: the `analysis_orders` query below this view feeds the bar chart of
the `supplier-risk-report` (Peacock). The orders themselves stay markdown pages; this is their line items
as data, read-only.
