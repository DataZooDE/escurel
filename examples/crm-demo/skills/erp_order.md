---
type: skill
id: erp_order
description: ERP sales orders, mirrored read-only through a sql_view backend over the shipped sources/erp JSON extract. Instances are materialised views — never authored as markdown.
backend:
  kind: sql_view
  source:
    connector: json_dir
    # Repo-root-relative on purpose: DuckDB resolves the glob against the
    # server process cwd. `scripts/demo-setup.sh` rewrites this to the
    # absolute path before materialising, so the demo works from any cwd.
    relation: examples/crm-demo/sources/erp
  project:
    customer: customer
    status: status
    amount_eur: amount_eur
  search_text: [customer, status]
optional_frontmatter: [customer, status, amount_eur]
---

# erp_order

A **sql_view-backed** skill: its instances are read-only DuckDB views over
an external source — here the `sources/erp/*.json` order extract shipped
with the demo (the offline stand-in for a live postgres/mysql/SAP
attachment). You never author an `erp_order` instance page; an admin
materialises one with the `create_sql_instance` tool and escurel writes the
overlay page + `backend_ref` binding itself.

Each order row carries:

- `order_id` — ERP document number (`SO-…`)
- `customer` — the CRM `[[customer::*]]` slug the order belongs to
- `amount_eur` — order value
- `status` — `open` / `invoiced` / `paid` / `overdue`
- `due_date` — payment due date

`expand` on the materialised instance returns the overlay body first, then
a **bounded projection** of the view's rows, with the projected columns
also exposed under the `source.<field>` namespace so overlay↔source drift
stays visible. `update_page` against the instance is rejected with
`backend_read_only` — the ERP extract is canonical.
