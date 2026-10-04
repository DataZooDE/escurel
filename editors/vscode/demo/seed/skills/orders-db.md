---
kind: skill
id: orders-db
title: Orders database
folder: sales/orders
role: record
tags: [sql, database]
description: The sales orders of the shop's SQL database (one instance per row, read live). The status and the quantity can be changed through a reviewed write-back.
autonomy: review
fields:
  - {name: order_no, kind: string, required: true, label: "Order"}
  - {name: customer, kind: string, label: "Customer"}
  - {name: material, kind: string, label: "Material"}
  - {name: quantity, kind: int, label: "Quantity"}
  - {name: net_value, kind: float, label: "Net value"}
  - {name: status, kind: enum, values: [open, shipped, on_hold, cancelled], label: "Status", render: badge}
backend:
  kind: sql_view
  instances: rows
  key: order_no
  linked: markdown
  filterable: [customer]
  writable_columns: [status, quantity]
  source: {connector: sqlite, attach: orders_db, relation: "main.orders"}
  project: {order_no: order_no, customer: customer, material: material, qty: quantity, net_value: net_value, status: status}
---

# orders-db

One instance per row of the `orders` table of a SQL database. The columns are the database's and
read-only here, except `status` and `quantity`: a change is proposed, a reviewer approves it, and only
then does escurel run one UPDATE on that row (after checking the row has not changed meanwhile).
