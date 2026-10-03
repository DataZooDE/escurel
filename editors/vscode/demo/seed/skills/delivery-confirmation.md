---
kind: skill
id: delivery-confirmation
title: Delivery confirmation
folder: purchasing/suppliers
role: record
tags: [mcp, external, supplier-portal]
description: A supplier's confirmation of one purchase-order line, read from the supplier portal's MCP server (outside escurel). The confirmation status can be changed through a reviewed write-back.
autonomy: review
fields:
  - {name: po, kind: string, label: "Purchase order"}
  - {name: material, kind: string, label: "Material"}
  - {name: qty_ordered, kind: int, label: "Quantity ordered"}
  - {name: qty_confirmed, kind: int, label: "Quantity confirmed"}
  - {name: confirmed_date, kind: date, label: "Confirmed delivery", render: date}
  - {name: status, kind: enum, values: [open, confirmed, moved], label: "Status", render: badge}
backend:
  kind: mcp
  endpoint: confirmations_mcp
  instances: rows
  key: $.id
  linked: true
  writable_columns: [status]
  list: {tool: listConfirmations, items: $.confirmations, limit_param: limit, cursor: {arg: after, from: $.next}}
  read: {tool: getConfirmation}
  write: {tool: updateConfirmation, idempotency_arg: idempotency_key}
  project: {po: $.po, material: $.material, qty_ordered: $.qty_ordered, qty_confirmed: $.qty_confirmed, confirmed_date: $.confirmed_date, status: $.status}
---

# delivery-confirmation

One instance per purchase-order line confirmation of the supplier portal's MCP server. The columns are
the portal's and read-only here, except `status`: a change is proposed, a reviewer approves it, and
only then does escurel call the portal's `updateConfirmation` tool (with an idempotency key).
