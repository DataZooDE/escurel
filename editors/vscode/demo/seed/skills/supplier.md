---
kind: skill
id: supplier
title: Supplier
folder: purchasing/suppliers
role: record
tags: [sap, mm]
description: A vendor we buy from (SAP MM, transaction XK03 / BP) - master data and how dependable its confirmations have been.
autonomy: review
required_frontmatter: [vendor, name, country, rating]
fields:
  - {name: vendor, kind: int, required: true, label: "Vendor (LIFNR)"}
  - {name: name, kind: string, required: true, label: "Name (NAME1)"}
  - {name: city, kind: string, label: "City (ORT01)"}
  - {name: country, kind: string, required: true, label: "Country (LAND1)"}
  - {name: purchasing_org, kind: string, label: "Purchasing organisation (EKORG)"}
  - {name: payment_terms, kind: string, label: "Payment terms (ZTERM)"}
  - {name: rating, kind: enum, values: [A, B, C], required: true, label: "Delivery reliability", render: badge}
backend:
  kind: sql_view
  instances: rows
  key: supplier_id
  linked: markdown
  filterable: [lifnr]
  source: {connector: json_dir, relation: "@LFA1_DIR@"}
  project: {lifnr: vendor, name1: name, ort01: city, land1: country, ekorg: purchasing_org, zterm: payment_terms, rating: rating}
---

# supplier

One instance per row of the LFA1 extract (the vendor's notes are its linked markdown):  who they are, what they deliver, how dependable their purchase-order
confirmations have been. A supplier-risk signal about a vendor changes the sales orders that depend
on it.
