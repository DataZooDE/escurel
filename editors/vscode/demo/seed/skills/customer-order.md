---
kind: skill
id: customer-order
description: A customer sales order (SAP SD, transaction VA03) - header data, delivery status and the items behind it.
autonomy: review
actions:
  - name: assess-supplier-risk
    kind: event
    label: Assess supplier risk
    event: supplier-risk
required_frontmatter: [sales_doc, order_type, sold_to, sold_to_name, sales_org, overall_status]
fields:
  - {name: sales_doc, kind: int, required: true, label: "Sales document (VBELN)"}
  - {name: order_type, kind: enum, values: [OR, ZOR, ZRK], required: true, label: "Order type (AUART)", render: badge}
  - {name: sold_to, kind: int, required: true, label: "Sold-to party (KUNNR)"}
  - {name: sold_to_name, kind: string, required: true, label: "Customer"}
  - {name: sales_org, kind: string, required: true, label: "Sales organisation (VKORG)"}
  - {name: po_number, kind: string, label: "Customer PO (BSTNK)"}
  - {name: plant, kind: string, label: "Delivering plant (WERKS)"}
  - {name: net_value, kind: float, min: 0, label: "Net value (NETWR)", render: money}
  - {name: currency, kind: string, label: "Currency (WAERK)"}
  - {name: requested_delivery, kind: date, label: "Requested delivery (VDATU)", render: date}
  - {name: confirmed_delivery, kind: date, label: "Confirmed delivery (EDATU)", render: date}
  - {name: overall_status, kind: enum, values: [open, partial, complete], required: true, label: "Overall status (GBSTK)", render: badge}
  - {name: delivery_block, kind: enum, values: [none, credit, supply], label: "Delivery block (LIFSK)", render: badge}
  - {name: delivery_risk, kind: enum, values: [low, medium, high], label: "Delivery risk", render: badge}
---

# customer-order

A customer sales order as it lives in SAP SD: the header (sold-to party, order type, sales
organisation, customer PO), its status (overall, delivery block) and the items with their
confirmed delivery dates. A signal about a vendor that supplies one of the materials changes
`delivery_risk` and, if the confirmation moves, `confirmed_delivery`, and is noted under
**History**.

Document numbers are integers on purpose: SAP shows them zero-padded (`0004500123`), the page
keeps the number.
